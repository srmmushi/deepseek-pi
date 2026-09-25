//! 中英双语文案
//!
//! 用法：`tr(lang, "key")` 取静态文案，需要插值时在调用处 `format!`。

use crate::config::Lang;

/// 取文案；未知 key 返回 key 本身（便于发现遗漏）。
///
/// 返回值的生命周期绑定到入参：命中时返回的是 `'static` 字面量（可自动降级），
/// 未命中时原样返回 `key`，所以不能声明为 `'static`。
pub fn tr<'a>(lang: Lang, key: &'a str) -> &'a str {
    match lang {
        Lang::Zh => zh(key),
        Lang::En => en(key),
    }
}

/// 开 / 关
pub fn on_off(lang: Lang, value: bool) -> &'static str {
    match (lang, value) {
        (Lang::Zh, true) => "开",
        (Lang::Zh, false) => "关",
        (Lang::En, true) => "on",
        (Lang::En, false) => "off",
    }
}

fn zh<'a>(key: &'a str) -> &'a str {
    match key {
        "app.notLoggedIn" => "尚未登录 · 执行 /login 录入 token",
        "status.thinking" => "深度思考",
        "status.search" => "智能搜索",
        "ui.loginMissing" => "未登录 · 输入 /login",
        "ui.hint" => "Enter 发送 · / 命令 · ! shell · Ctrl+T 思考 · Ctrl+S 搜索 · Ctrl+O 展开思考 · Ctrl+↑/↓ 滚动 · 拖动选择、右键复制 · Esc 退出",
        "repl.stopped" => "已中断",
        "tool.write" => "已写入",
        "tool.exec" => "命令退出码",
        "tool.crashed" => "工具执行异常",
        "tool.lines" => "行",
        "tool.entries" => "项",
        "tool.matches" => "处匹配",
        "shell.usage" => "用法：!<命令>，例如 !git status",
        "shell.exit" => "exit",
        "login.failed" => "登录失败",
        "cmd.clear" => "已重置当前会话上下文。",
        "cmd.status" => "状态",
        "cmd.langSet" => "界面语言已切换。",
        "toggle.thinking" => "深度思考",
        "toggle.search" => "智能搜索",
        "error.sessionExpired" => "凭证已失效，请重新 /login",
        "error.rateLimit" => "触发上游限流，请稍后再试。",
        "error.waf" => "被 CloudFront WAF 拦截（常见于美国 IP），请配置非美国地区代理。",
        "error.pow" => "PoW 求解失败（上游 WASM 可能已更新）",
        "error.http" => "网络请求失败",
        "error.apiChanged" => "接口返回异常（上游可能已变更）",
        _ => key,
    }
}

fn en<'a>(key: &'a str) -> &'a str {
    match key {
        "app.notLoggedIn" => "not signed in · run /login to set the token",
        "status.thinking" => "thinking",
        "status.search" => "search",
        "ui.loginMissing" => "not signed in · run /login",
        "ui.hint" => "Enter send · / commands · ! shell · Ctrl+T thinking · Ctrl+S search · Ctrl+O expand thinking · Ctrl+↑/↓ scroll · drag to select, right-click to copy · Esc quit",
        "repl.stopped" => "interrupted",
        "tool.write" => "wrote",
        "tool.exec" => "exit code",
        "tool.crashed" => "tool crashed",
        "tool.lines" => "lines",
        "tool.entries" => "entries",
        "tool.matches" => "matches",
        "shell.usage" => "Usage: !<command>, e.g. !git status",
        "shell.exit" => "exit",
        "login.failed" => "login failed",
        "cmd.clear" => "Session context reset.",
        "cmd.status" => "status",
        "cmd.langSet" => "UI language switched.",
        "toggle.thinking" => "thinking",
        "toggle.search" => "search",
        "error.sessionExpired" => "credential expired; run /login again",
        "error.rateLimit" => "upstream rate limited; retry later.",
        "error.waf" => "blocked by CloudFront WAF (usually a US IP); configure a non-US proxy.",
        "error.pow" => "PoW failed (upstream WASM may have changed)",
        "error.http" => "network request failed",
        "error.apiChanged" => "unexpected API response (upstream may have changed)",
        _ => key,
    }
}
