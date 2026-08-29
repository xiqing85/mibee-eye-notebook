// Build script: bundle the Preact SPA and inline CSS+JS into index.html.
//
// Pipeline:
//   1. Run esbuild (vendored at tools/esbuild-linux-x64) to bundle
//      static/src/app.jsx + its imports (preact, preact/signals) into a single
//      minified static/app.bundle.js. On non-Linux hosts esbuild is skipped
//      and a stale bundle is reused (or the build errors — Linux-only is the
//      documented platform).
//   2. Read static/index.template.html, replace the <style id="injected-css">
//      placeholder with the contents of static/style.css, and the
//      <script id="injected-js"> placeholder with the bundled JS.
//   3. Write the final static/index.html (gitignored).
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = PathBuf::from(&manifest_dir);

    println!("cargo:rerun-if-changed=static/src/app.jsx");
    println!("cargo:rerun-if-changed=static/src");
    println!("cargo:rerun-if-changed=static/style.css");
    println!("cargo:rerun-if-changed=static/index.template.html");
    println!("cargo:rerun-if-changed=../package.json");

    // Step 1: esbuild bundle (Linux only — matches the project's Tier-1 scope).
    let entry = base.join("static/src/app.jsx");
    let bundle_out = base.join("static/app.bundle.js");
    if cfg!(target_os = "linux") && entry.exists() {
        // The web crate lives at <repo>/crates/web; tools/ is at <repo>/tools.
        // Walk up two levels to find the workspace root.
        let repo_root = base
            .parent() // crates/
            .and_then(|p| p.parent()) // repo root
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| base.clone());
        let workspace_esbuild = repo_root.join("tools/esbuild-linux-x64");
        let esbuild_path = if workspace_esbuild.exists() {
            workspace_esbuild.to_string_lossy().to_string()
        } else {
            "esbuild".to_string()
        };

        // node_modules may live at <repo>/node_modules (npm install root) or
        // <repo>/crates/web/static/node_modules (vendored). esbuild resolves
        // upward from the entry file, so both are discoverable; we just make
        // sure NODE_PATH also points there as a fallback.
        let nm_candidates = [
            repo_root.join("node_modules"),
            base.join("static/node_modules"),
        ];
        let node_paths: Vec<String> = nm_candidates
            .iter()
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut cmd = Command::new(&esbuild_path);
        cmd.arg(&entry)
            .arg("--bundle")
            .arg("--format=iife")
            .arg("--target=es2020,chrome80,firefox75,safari14")
            .arg("--minify")
            .arg("--loader:.js=jsx")
            .arg("--jsx=automatic")
            .arg("--jsx-import-source=preact")
            .arg("--define:process.env.NODE_ENV=\"production\"")
            .arg(format!("--outfile={}", bundle_out.display()));
        if !node_paths.is_empty() {
            // NODE_PATH lets esbuild resolve preact without a relative import.
            cmd.env("NODE_PATH", node_paths.join(":"));
        }
        match cmd.output() {
            Ok(out) => {
                if !out.status.success() {
                    panic!(
                        "esbuild bundling failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr),
                    );
                }
            }
            Err(e) => {
                panic!(
                    "failed to invoke esbuild at {esbuild_path}: {e}. \
                        Ensure tools/esbuild-linux-x64 is present (Linux-only build)."
                );
            }
        }
    } else if !bundle_out.exists() {
        // Non-Linux dev host with no prebuilt bundle — emit a placeholder so
        // the build still produces an index.html (the SPA won't work, but the
        // Rust crate compiles). Real builds happen on Linux.
        std::fs::write(
            &bundle_out,
            "console.error('SPA bundle not built — run on Linux.');",
        )
        .expect("write placeholder bundle");
    }

    // Step 2: inline CSS + JS into the HTML template.
    let template =
        std::fs::read_to_string(base.join("static/index.template.html")).expect("template");
    let js = std::fs::read_to_string(&bundle_out).expect("app.bundle.js");
    let css = std::fs::read_to_string(base.join("static/style.css")).expect("style.css");
    let output = template
        .replace(
            "<style id=\"injected-css\"></style>",
            &format!("<style>\n{}\n</style>", css),
        )
        .replace(
            "<script id=\"injected-js\"></script>",
            &format!("<script>\n{}\n</script>", js),
        );
    std::fs::write(base.join("static/index.html"), output).expect("write index.html");
}
