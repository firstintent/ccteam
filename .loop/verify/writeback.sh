#!/usr/bin/env bash
# .loop/verify/writeback.sh — 写权守卫 + backlog 结构校验(dev 收口必跑)
#
# 用法:
#   .loop/verify/writeback.sh <base-sha>   # 收口检查:base = 开工时的 HEAD
#   .loop/verify/writeback.sh --selftest   # 守卫自证:每类坏样例必红 + 合法样例必绿
#
# 语义与 AGENTS.md §五「角色与写权」逐条对齐:
#   1) 保护路径:AGENTS.md / CLAUDE.md / docs/** / .github/workflows/** / .loop/**(除 backlog.md)
#      在 base..工作区(含未跟踪)被改 ⇒ 红。卡面显式授权某路径时 WRITEBACK_ALLOW="<前缀>…";
#      规划(控制)会话改治理面用 WRITEBACK_ALLOW='*' 跳过路径守卫(结构校验仍跑)。
#   2) backlog 结构:每张 `### ` 卡必有状态行(ASCII 冒号);状态词闭合:
#      待排 | 进行中(…) | 完成(7+位hex) | 阻塞(…) | gated[(…)];完成卡必有「验证」段;
#      同冲突域双「进行中」⇒ 红;游离状态行 ⇒ 红。
#   3) 真仓绿检:对当前真实 .loop/backlog.md 的结构校验永远执行(防守卫与现实脱节)。
set -euo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
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

run_guard() { # $1 = base sha
    cd "$(git rev-parse --show-toplevel)"
    git rev-parse --verify -q "$1^{commit}" >/dev/null || fail "base sha 无效:$1"
    local allow="${WRITEBACK_ALLOW:-}"
    if [ "$allow" = "*" ]; then
        echo "writeback: 路径守卫跳过(WRITEBACK_ALLOW='*',规划会话语义)"
    else
        local viol="" f a ok
        while IFS= read -r f; do
            [ -n "$f" ] || continue
            case "$f" in
                .loop/backlog.md) continue ;;
                AGENTS.md | CLAUDE.md | docs/* | .github/workflows/* | .loop/*)
                    ok=0
                    for a in $allow; do case "$f" in "$a"*) ok=1 ;; esac; done
                    [ "$ok" = 1 ] || viol="$viol $f"
                    ;;
            esac
        done <<<"$({ git diff --name-only "$1" --; git ls-files --others --exclude-standard; } | sort -u)"
        [ -z "$viol" ] || fail "保护路径被改(治理面 = 规划会话专属;卡面授权走 WRITEBACK_ALLOW):$viol"
    fi
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

    # 保护路径守卫:临时 git 仓,base 后改 docs/ ⇒ 红;只改代码 + backlog ⇒ 绿
    (
        cd "$tmp" && git init -q repo && cd repo
        git config user.email t@t && git config user.name t
        mkdir -p docs .loop src
        cp "$tmp/good.md" .loop/backlog.md
        echo x >docs/a.md && echo fn >src/lib.rs
        git add -A && git commit -qm base
        base="$(git rev-parse HEAD)"
        echo y >>docs/a.md
        if WRITEBACK_ALLOW='' "$SCRIPT" "$base" >/dev/null 2>&1; then
            echo "selftest: FAIL(保护路径应红未红)" >&2; exit 1
        fi
        git checkout -q -- docs/a.md
        echo more >>src/lib.rs && echo add >>.loop/backlog.md
        WRITEBACK_ALLOW='' "$SCRIPT" "$base" >/dev/null 2>&1 || { echo "selftest: FAIL(合法收口被误红)" >&2; exit 1; }
    ) || exit 1
    red=$((red + 1)) pass=$((pass + 1))

    echo "writeback selftest: GREEN(合法 ${pass} 绿 + 坏样例 ${red} 红)"
}

case "${1:-}" in
    --selftest) selftest ;;
    "") fail "用法:writeback.sh <base-sha> | --selftest" ;;
    *) run_guard "$1" ;;
esac
