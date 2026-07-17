#!/usr/bin/env bash
# .loop/verify/writeback.sh — backlog 队列结构校验(收口必跑)
#
# 用法:
#   .loop/verify/writeback.sh             # 校验 .loop/backlog.md 结构
#   .loop/verify/writeback.sh --selftest  # 自证:每类坏样例必红 + 合法样例必绿
#
# 治理写权(AGENTS.md §五「角色与写权」)= 声明 + Fable 5 规划会话复核执法,
# **不做脚本硬防护**(owner 决策 2026-07-17;此前的保护路径守卫已删)。
# 本脚本只守队列结构,语义与 backlog 文件头协议对齐:
#   每张 `### ` 卡必有状态行(ASCII 冒号);状态词闭合:
#   待排 | 进行中(…) | 完成(7+位hex) | 阻塞(…) | gated[(…)];完成卡必有「验证」段;
#   同冲突域双「进行中」⇒ 红(冲突域首段 = 路径前缀,前缀重叠即同域);游离状态行 ⇒ 红。
set -euo pipefail

BACKLOG=".loop/backlog.md"

fail() { echo "writeback: RED — $*" >&2; exit 1; }

check_backlog() { # $1 = backlog 文件;结构违规打印诊断并返回非零
    awk '
        function flush_card() {
            if (in_card && !has_status) { printf "  卡「%s」缺状态行(- **状态**:…,ASCII 冒号)\n", title; bad = 1 }
            if (in_card && status ~ /^完成/ && !has_verify) { printf "  完成卡「%s」缺「- **验证**」段\n", title; bad = 1 }
        }
        BEGIN { in_card = 0; bad = 0 }
        /^### / { flush_card(); in_card = 1; has_status = 0; has_verify = 0; status = ""; title = substr($0, 5); next }
        /^## /  { flush_card(); in_card = 0; next }
        /^- \*\*状态\*\*:/ {
            if (!in_card) { printf "  游离状态行(不在任何 ### 卡内):%s\n", $0; bad = 1; next }
            has_status = 1
            line = $0; sub(/^- \*\*状态\*\*:/, "", line)
            n = index(line, " · "); if (n > 0) status = substr(line, 1, n - 1); else status = line
            gsub(/^[ \t]+/, "", status); gsub(/[ \t]+$/, "", status)
            if (status !~ /^(待排|进行中\([^)]+\)|完成\([0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*\)|阻塞\([^)]+\)|gated(\([^)]*\))?)$/) {
                printf "  卡「%s」状态词不在闭合集:%s\n", title, status; bad = 1
            }
            if (status ~ /^进行中/) {
                cd = ""
                if (match($0, /\*\*冲突域\*\*:`[^`]+`/)) {
                    cd = substr($0, RSTART, RLENGTH)
                    sub(/^\*\*冲突域\*\*:`/, "", cd); sub(/`$/, "", cd)
                }
                if (cd != "") { if (cd in wip) { printf "  冲突域 `%s` 有两张「进行中」卡(同域须串行)\n", cd; bad = 1 } wip[cd] = 1 }
            }
            next
        }
        /^- \*\*验证\*\*/ { if (in_card) has_verify = 1; next }
        END { flush_card(); exit bad }
    ' "$1"
}

run_check() {
    cd "$(git rev-parse --show-toplevel)"
    [ -f "$BACKLOG" ] || fail "缺 $BACKLOG"
    check_backlog "$BACKLOG" || fail "backlog 结构校验失败(见上)"
    echo "writeback: GREEN"
}

selftest() {
    local tmp pass=0 red=0
    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064 — 有意在设 trap 时展开(tmp 是函数局部,EXIT 时已出作用域)
    trap "rm -rf '$tmp'" EXIT

    expect_red() { # $1 = 名字, $2 = 文件
        if check_backlog "$2" >/dev/null 2>&1; then
            echo "selftest: FAIL(应红未红)$1" >&2; exit 1
        fi
        red=$((red + 1))
    }

    cat >"$tmp/good.md" <<'EOF'
## 当前卡

### T1 示例卡
- **状态**:待排 · **冲突域**:`crates/x`
- **规格**:demo

### T2 进行中例
- **状态**:进行中(cc·2026-07-17) · **冲突域**:`crates/y`

### T3 完成例
- **状态**:完成(abcdef0) · **冲突域**:`crates/z`
- **验证**:make test 绿
EOF
    check_backlog "$tmp/good.md" || { echo "selftest: FAIL(合法样例被误红)" >&2; exit 1; }
    pass=$((pass + 1))

    sed '/T1 示例卡/,/^$/{/状态/d}' "$tmp/good.md" >"$tmp/bad-nostatus.md"
    expect_red "缺状态行" "$tmp/bad-nostatus.md"
    sed 's/待排/搞定了/' "$tmp/good.md" >"$tmp/bad-vocab.md"
    expect_red "状态词越界" "$tmp/bad-vocab.md"
    sed 's|`crates/x`|`crates/y`|; s/:待排/:进行中(codex·2026-07-17)/' "$tmp/good.md" >"$tmp/bad-dup.md"
    expect_red "同冲突域双进行中" "$tmp/bad-dup.md"
    sed '/make test 绿/d' "$tmp/good.md" >"$tmp/bad-noverify.md"
    expect_red "完成卡缺验证段" "$tmp/bad-noverify.md"

    echo "writeback selftest: GREEN(合法 ${pass} 绿 + 坏样例 ${red} 红)"
}

case "${1:-}" in
    --selftest) selftest ;;
    "") run_check ;;
    *) fail "用法:writeback.sh(无参数 = 结构校验)| --selftest" ;;
esac
