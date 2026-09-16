#!/usr/bin/env bash
# =============================================================================
# 硬编码门禁（2026-09-15 规范）：产品代码中不得出现应配置化的硬编码值。
#
# 用法：
#   tools/check-hardcode.sh            # 全量模式：检查所有已跟踪源文件（CI 用）
#   tools/check-hardcode.sh --staged   # 只检查本次 staged 的源文件（pre-commit 用）
#
# 退出码：0 = 通过；1 = 存在违规。
# 规则来源：工作区 AGENTS.md「硬编码门禁」的可执行化——值必须可配置：
#   设备路径、私网 IP、版本字符串、国标设备 ID、外部可执行名。
# 默认值集中声明在配置模块（本仓：src/config.rs、crates/web/src/config.rs），
# 业务代码只读配置；测试代码不适用（golden 语义钉板）。
# 豁免：确属语义常量/技术探测等 → 在同一行加 `hardcode-ok: <理由>` 标注，
#   并在提交说明中写明理由（同卫生门禁的误报处理文化）。
# =============================================================================
set -u

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd) || { echo "✗ 无法定位脚本目录"; exit 1; }
root=$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null) || { echo "✗ 不在 git 仓库内"; exit 1; }
cd "$root"

mode=all
[ "${1:-}" = "--staged" ] && mode=staged

if [ "$mode" = staged ]; then
    mapfile -d '' files < <(git diff --cached --name-only --diff-filter=ACMR -z -- '*.rs' '*.go')
else
    mapfile -d '' files < <(git ls-files -z -- '*.rs' '*.go')
fi
[ ${#files[@]} -eq 0 ] && { echo "✓ 硬编码门禁通过（无可检源文件）"; exit 0; }

fail=0
checked=0

# 语义常量豁免（本行出现即跳过 IP 检查）：全网监听与回环是绑定/回退语义，
# 不是部署值；其余 IP 一律过配置。
ip_semantic='0\.0\.0\.0|127\.0\.0\.1|255\.255\.255\.255|multicast'

for f in "${files[@]}"; do
    case "$f" in
        # 配置模块：默认值的唯一合法声明处
        src/config.rs|crates/web/src/config.rs) continue ;;
        # vendored 第三方补丁/生成目录：不适用本仓门禁
        .cargo/patches/*|*/vendor/*|*/static/*|*/testdata/*|*/migrations/*) continue ;;
        # 测试：golden 语义钉板，不适用本门禁
        *_test.go|*/tests/*|tests/*) continue ;;
    esac
    [ -f "$f" ] || continue
    checked=$((checked + 1))

    # Rust：#[cfg(test)] 之后的行是内联测试模块，跳过（约定测试在文件尾部）
    in_rust_test=0
    line_no=0
    while IFS= read -r line || [ -n "$line" ]; do
        line_no=$((line_no + 1))
        case "$f" in
        *.rs)
            case "$line" in
            '#[cfg(test)]'*) in_rust_test=1 ;;
            esac
            [ "$in_rust_test" = 1 ] && continue
            ;;
        esac

        # 行内豁免标注
        case "$line" in *hardcode-ok*) continue ;; esac
        # 注释行（Rust/Go 单行注释开头）
        trimmed="${line#"${line%%[![:space:]]*}"}"
        case "$trimmed" in '//'*) continue ;; '#'*) continue ;; esac

        # ── 规则 1：设备节点路径必须来自配置 ──
        if printf '%s' "$line" | grep -qE '/dev/(video|media|dri/)'; then
            echo "✗ $f:$line_no 设备节点硬编码：$line"
            echo "  → 设备路径走配置"
            fail=1
        fi

        # ── 规则 2：私网/具体 IP 字面量（语义常量豁免除外）──
        if printf '%s' "$line" | grep -qE '(^|[^0-9.])((192\.168|10\.[0-9]+|172\.(1[6-9]|2[0-9]|3[01]))\.[0-9]+\.[0-9]+|[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3})([^0-9.]|$)' \
           && ! printf '%s' "$line" | grep -qE "$ip_semantic"; then
            echo "✗ $f:$line_no IP 字面量：$line"
            echo "  → 地址走配置；探测/回环语义加行内 hardcode-ok: <理由> 豁免"
            fail=1
        fi

        # ── 规则 3：版本字符串字面量（横幅 bug 类）──
        if printf '%s' "$line" | grep -qE '"v?[0-9]+\.[0-9]+\.[0-9]+"'; then
            echo "✗ $f:$line_no 版本字符串硬编码：$line"
            echo "  → 用 CARGO_PKG_VERSION / 构建注入变量"
            fail=1
        fi

        # ── 规则 4：国标设备/平台 ID 字面量 ──
        if printf '%s' "$line" | grep -qE '"[0-9]{20}"'; then
            echo "✗ $f:$line_no 20 位国标 ID 硬编码：$line"
            echo "  → 设备/通道/平台 ID 走配置"
            fail=1
        fi

        # ── 规则 5：外部可执行名（默认值只在配置模块声明）──
        if printf '%s' "$line" | grep -qE '"(ffmpeg|ffprobe|rpicam-still|rpicam-vid|mtxrpicam)"'; then
            echo "✗ $f:$line_no 外部可执行名硬编码：$line"
            echo "  → 二进制路径/名称走配置"
            fail=1
        fi
    done < "$f"
done

if [ "$fail" = 1 ]; then
    echo
    echo "✗ 硬编码门禁未通过（模式: $mode，检查 $checked 个文件）。"
    echo "  规范：值必须可配置——默认值集中在配置模块，业务代码只读配置；"
    echo "  确属语义常量 → 行内 hardcode-ok: <理由> 并在提交说明中写明。"
    exit 1
fi
echo "✓ 硬编码门禁通过（模式: $mode，检查 $checked 个文件）"
