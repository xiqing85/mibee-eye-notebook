//! GFL (Generalized Focal Loss) post-processing for NanoDet ONNX models.
//!
//! Ported from `mibee-eye-raspi-rs` (same model, same semantics) so all
//! MiBee cameras decode identically:
//!
//! 1. **Classification** — the first `NUM_CLASSES` channels per point are
//!    class scores (the `nanodet-plus-m_320.onnx` export folds the sigmoid
//!    into the graph); argmax picks the label.
//! 2. **GFL regression** — the remaining `4 × (REG_MAX + 1)` channels hold a
//!    discrete distance distribution per box side (l, t, r, b); the expected
//!    distance is `dot(softmax(bins), [0..=REG_MAX])` in stride units.
//! 3. **Bbox** — `x1 = (grid_x - l) * stride`, … clamped to the input frame.
//! 4. **NMS** — per-class non-maximum suppression at IoU 0.5.
//!
//! Model output layout: `[1, 2125, 112]` (point-major) — 2125 points =
//! 40² + 20² + 10² + 5² at strides [8, 16, 32, 64]; 112 channels = 80 COCO
//! classes + 32 regression.

use super::Detection;
use anyhow::{Result, bail};

/// Number of COCO object classes.
const NUM_CLASSES: usize = 80;
/// Maximum bin index of the GFL regression distribution (8 bins: 0..=7).
const REG_MAX: usize = 7;
/// Number of distance bins per box side.
const NUM_BINS: usize = REG_MAX + 1;
/// Regression channels: 4 sides × 8 bins.
const NUM_REGRESSION: usize = 4 * NUM_BINS;
/// Total channels per grid point.
const NUM_CHANNELS: usize = NUM_CLASSES + NUM_REGRESSION;
/// Model input size (square, 320×320).
const INPUT_SIZE: u32 = 320;
/// Feature-map strides of the FPN levels.
const STRIDES: [u32; 4] = [8, 16, 32, 64];
/// Feature-map width/height per stride level (320 / stride).
const GRID_SIZES: [usize; 4] = [40, 20, 10, 5];
/// Cumulative point offset of each stride level.
const LEVEL_OFFSETS: [usize; 4] = [0, 1600, 2000, 2100];
/// Total number of grid points across all levels.
const NUM_POINTS: usize = 2125;
/// NMS intersection-over-union threshold.
const NMS_IOU_THRESHOLD: f32 = 0.5;

/// COCO 80-class labels in model output order (index = class id).
const COCO_LABELS: [&str; NUM_CLASSES] = [
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
];

/// A decoded detection candidate before NMS.
#[derive(Debug, Clone)]
struct Candidate {
    label: usize,
    confidence: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

/// Decode the ONNX output tensor into detections.
///
/// `output` is the flat `[1, 2125, 112]` tensor data; `confidence_threshold`
/// is the pre-NMS filter applied to the (already sigmoid'd) class scores.
pub fn postprocess(output: &[f32], confidence_threshold: f32) -> Result<Vec<Detection>> {
    if output.len() != NUM_POINTS * NUM_CHANNELS {
        bail!(
            "Unexpected ONNX output length: got {}, expected {} ({} points × {} channels)",
            output.len(),
            NUM_POINTS * NUM_CHANNELS,
            NUM_POINTS,
            NUM_CHANNELS
        );
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    for (point_idx, point) in output.chunks_exact(NUM_CHANNELS).enumerate() {
        let (_level, stride, grid_x, grid_y) = grid_coords(point_idx);

        // Classification: argmax over the class channels.
        let mut label = 0usize;
        let mut confidence = point[0];
        for (class, &score) in point[..NUM_CLASSES].iter().enumerate().skip(1) {
            if score > confidence {
                label = class;
                confidence = score;
            }
        }
        if confidence < confidence_threshold {
            continue;
        }

        let distances = decode_distances(&point[NUM_CLASSES..]);

        let x1 = (grid_x as f32 - distances[0]) * stride as f32;
        let y1 = (grid_y as f32 - distances[1]) * stride as f32;
        let x2 = (grid_x as f32 + distances[2]) * stride as f32;
        let y2 = (grid_y as f32 + distances[3]) * stride as f32;

        candidates.push(Candidate {
            label,
            confidence,
            x1: x1.max(0.0),
            y1: y1.max(0.0),
            x2: x2.min(INPUT_SIZE as f32),
            y2: y2.min(INPUT_SIZE as f32),
        });
    }

    candidates.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let kept = nms(&candidates, NMS_IOU_THRESHOLD);

    Ok(kept
        .into_iter()
        .map(|c| Detection {
            label: COCO_LABELS[c.label].to_string(),
            confidence: c.confidence,
            bbox: [
                c.x1.round() as u32,
                c.y1.round() as u32,
                (c.x2 - c.x1).round() as u32,
                (c.y2 - c.y1).round() as u32,
            ],
        })
        .collect())
}

/// Scale detections from model-input pixel space back to the source video
/// frame's pixel space (SPEC v1 §4.6: bboxes are in **video pixel**
/// coordinates). The preprocessing stretches the frame into the square
/// model input without letterboxing, so x and y scale independently.
/// Boxes are clamped to the frame bounds so rounding cannot push an edge
/// one pixel outside.
pub fn scale_detections_to_frame(
    detections: Vec<Detection>,
    model_w: u32,
    model_h: u32,
    frame_w: u32,
    frame_h: u32,
) -> Vec<Detection> {
    if model_w == 0 || model_h == 0 || frame_w == 0 || frame_h == 0 {
        return detections;
    }
    let scale_x = frame_w as f32 / model_w as f32;
    let scale_y = frame_h as f32 / model_h as f32;
    detections
        .into_iter()
        .map(|mut det| {
            let [x, y, w, h] = det.bbox;
            let x = ((x as f32 * scale_x).round() as u32).min(frame_w.saturating_sub(1));
            let y = ((y as f32 * scale_y).round() as u32).min(frame_h.saturating_sub(1));
            let w = ((w as f32 * scale_x).round() as u32).min(frame_w - x);
            let h = ((h as f32 * scale_y).round() as u32).min(frame_h - y);
            det.bbox = [x, y, w, h];
            det
        })
        .collect()
}

/// Map a flat point index to its (level, stride, grid_x, grid_y).
///
/// Points are ordered level-major (stride 8 first) and row-major within
/// each level (y outer, x inner), matching NanoDet's
/// `generate_grid_center_priors`.
fn grid_coords(point_idx: usize) -> (usize, u32, usize, usize) {
    let level = LEVEL_OFFSETS
        .iter()
        .rposition(|&offset| offset <= point_idx)
        .unwrap_or(0);
    let offset = LEVEL_OFFSETS[level];
    let local = point_idx - offset;
    let grid_w = GRID_SIZES[level];
    (level, STRIDES[level], local % grid_w, local / grid_w)
}

/// Decode the 4 box-side distances (l, t, r, b) from the regression
/// channels: each side's bins form a discrete distribution over distances
/// `0..=REG_MAX` (stride units); the expected distance is the
/// softmax-weighted sum.
fn decode_distances(regression: &[f32]) -> [f32; 4] {
    let mut distances = [0.0f32; 4];
    for (side, distance) in distances.iter_mut().enumerate() {
        let bins = &regression[side * NUM_BINS..(side + 1) * NUM_BINS];
        let weights = softmax(bins);
        *distance = weights
            .iter()
            .enumerate()
            .map(|(bin, &weight)| bin as f32 * weight)
            .sum();
    }
    distances
}

/// Numerically stable softmax over a slice.
fn softmax(values: &[f32]) -> Vec<f32> {
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = values.iter().map(|&v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.iter().map(|&e| e / sum).collect()
}

/// Per-class non-maximum suppression.
///
/// Candidates must be sorted by confidence descending. A candidate is kept
/// if its IoU with every already-kept candidate of the same class is ≤
/// threshold.
fn nms(candidates: &[Candidate], iou_threshold: f32) -> Vec<Candidate> {
    let mut kept: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        let suppressed = kept
            .iter()
            .any(|k| k.label == candidate.label && iou(k, candidate) > iou_threshold);
        if !suppressed {
            kept.push(candidate.clone());
        }
    }
    kept
}

/// Intersection-over-union of two boxes.
fn iou(a: &Candidate, b: &Candidate) -> f32 {
    let inter_w = a.x2.min(b.x2) - a.x1.max(b.x1);
    let inter_h = a.y2.min(b.y2) - a.y1.max(b.y1);
    if inter_w <= 0.0 || inter_h <= 0.0 {
        return 0.0;
    }
    let inter = inter_w * inter_h;
    let union = (a.x2 - a.x1) * (a.y2 - a.y1) + (b.x2 - b.x1) * (b.y2 - b.y1) - inter;
    inter / union
}

// ── Tests (ported from mibee-eye-raspi-rs — same model, same numbers) ────────

#[cfg(test)]
mod tests {
    use super::*;

    mod scale_to_frame {
        use super::*;

        /// Real-world case: 1280×720 frame stretched into a 320×320 model
        /// input (x scale 4.0, y scale 2.25).
        #[test]
        fn test_maps_model_box_to_video_pixels() {
            let dets = vec![Detection {
                label: "chair".to_string(),
                confidence: 0.65,
                bbox: [272, 272, 48, 48],
            }];
            let scaled = scale_detections_to_frame(dets, 320, 320, 1280, 720);
            assert_eq!(scaled[0].bbox, [1088, 612, 192, 108]);
        }

        #[test]
        fn test_identity_when_sizes_match() {
            let dets = vec![Detection {
                label: "person".to_string(),
                confidence: 0.9,
                bbox: [10, 20, 30, 40],
            }];
            let scaled = scale_detections_to_frame(dets, 320, 320, 320, 320);
            assert_eq!(scaled[0].bbox, [10, 20, 30, 40]);
        }

        #[test]
        fn test_clamps_to_frame_bounds() {
            let dets = vec![Detection {
                label: "person".to_string(),
                confidence: 0.9,
                bbox: [300, 300, 30, 30],
            }];
            let scaled = scale_detections_to_frame(dets, 320, 320, 1280, 720);
            let [x, y, w, h] = scaled[0].bbox;
            assert_eq!((x, y), (1200, 675));
            assert!(y + h <= 720, "y+h={}", y + h);
            assert!(x + w <= 1280, "x+w={}", x + w);
        }
    }

    /// Class 0 ("person") at grid (x=20, y=20) on the stride-8 level
    /// (point 820), one-hot regression at distances (2, 3, 4, 5) → bbox
    /// (144, 136, 192, 200).
    fn synthetic_output_with_detection() -> Vec<f32> {
        let mut output = vec![0.0f32; NUM_POINTS * NUM_CHANNELS];
        let base = (20 * 40 + 20) * NUM_CHANNELS;
        output[base] = 0.9;
        for class in 1..NUM_CLASSES {
            output[base + class] = 0.01;
        }
        for (side, distance) in [2usize, 3, 4, 5].iter().enumerate() {
            output[base + NUM_CLASSES + side * NUM_BINS + distance] = 20.0;
        }
        output
    }

    #[test]
    fn test_synthetic_detection_decodes_bbox_within_2px() {
        let output = synthetic_output_with_detection();
        let detections = postprocess(&output, 0.4).expect("postprocess");
        assert_eq!(detections.len(), 1);
        let detection = &detections[0];
        assert_eq!(detection.label, "person");
        assert!((detection.confidence - 0.9).abs() < 1e-6);
        let [x, y, w, h] = detection.bbox;
        assert!((x as i32 - 144).abs() <= 2, "x={x}");
        assert!((y as i32 - 136).abs() <= 2, "y={y}");
        assert!((w as i32 - 48).abs() <= 2, "w={w}");
        assert!((h as i32 - 64).abs() <= 2, "h={h}");
    }

    #[test]
    fn test_all_zero_output_yields_no_detections() {
        let output = vec![0.0f32; NUM_POINTS * NUM_CHANNELS];
        assert!(postprocess(&output, 0.4).expect("postprocess").is_empty());
    }

    #[test]
    fn test_short_output_returns_error() {
        let output = vec![0.0f32; NUM_POINTS * NUM_CHANNELS - 1];
        let err = postprocess(&output, 0.4);
        assert!(err.is_err());
    }

    #[test]
    fn test_grid_coords() {
        assert_eq!(grid_coords(0), (0, 8, 0, 0));
        assert_eq!(grid_coords(820), (0, 8, 20, 20));
        assert_eq!(grid_coords(1599), (0, 8, 39, 39));
        assert_eq!(grid_coords(1600), (1, 16, 0, 0));
        assert_eq!(grid_coords(2000), (2, 32, 0, 0));
        assert_eq!(grid_coords(2100), (3, 64, 0, 0));
        assert_eq!(grid_coords(2124), (3, 64, 4, 4));
    }

    #[test]
    fn test_nms_suppresses_same_class_overlap() {
        let candidates = vec![
            Candidate {
                label: 0,
                confidence: 0.9,
                x1: 10.0,
                y1: 10.0,
                x2: 100.0,
                y2: 100.0,
            },
            Candidate {
                label: 0,
                confidence: 0.8,
                x1: 20.0,
                y1: 20.0,
                x2: 110.0,
                y2: 110.0,
            },
        ];
        let kept = nms(&candidates, 0.5);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].confidence, 0.9);
    }

    #[test]
    fn test_nms_keeps_different_classes() {
        let candidates = vec![
            Candidate {
                label: 0,
                confidence: 0.9,
                x1: 10.0,
                y1: 10.0,
                x2: 100.0,
                y2: 100.0,
            },
            Candidate {
                label: 2,
                confidence: 0.8,
                x1: 20.0,
                y1: 20.0,
                x2: 110.0,
                y2: 110.0,
            },
        ];
        let kept = nms(&candidates, 0.5);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn test_coco_labels() {
        assert_eq!(COCO_LABELS.len(), 80);
        assert_eq!(COCO_LABELS[0], "person");
        assert_eq!(COCO_LABELS[2], "car");
        assert_eq!(COCO_LABELS[16], "dog");
        assert_eq!(COCO_LABELS[79], "toothbrush");
    }
}
