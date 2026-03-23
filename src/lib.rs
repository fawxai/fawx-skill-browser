use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;

const INFO_LEVEL: u32 = 2;
const ERROR_LEVEL: u32 = 4;
const DEFAULT_MAX_LENGTH: usize = 10_000;
const DEFAULT_SEARCH_COUNT: usize = 5;
const MAX_SEARCH_COUNT: usize = 10;
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const EMPTY_JSON: &str = "{}";
const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const HOST_BINARY_BASE64_PREFIX: &str = "__fawx_binary_base64__:";
const MAX_HOST_STRING_LEN: usize = 1_048_576;

#[link(wasm_import_module = "host_api_v1")]
extern "C" {
    #[link_name = "log"]
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32);
    #[link_name = "get_input"]
    fn host_get_input() -> u32;
    #[link_name = "set_output"]
    fn host_set_output(text_ptr: *const u8, text_len: u32);
    #[link_name = "kv_get"]
    fn host_kv_get(key_ptr: *const u8, key_len: u32) -> u32;
    #[link_name = "http_request"]
    fn host_http_request(
        method_ptr: *const u8,
        method_len: u32,
        url_ptr: *const u8,
        url_len: u32,
        headers_ptr: *const u8,
        headers_len: u32,
        body_ptr: *const u8,
        body_len: u32,
    ) -> u32;
}

#[derive(Debug, Deserialize)]
struct BrowserInput {
    tool: Option<String>,
    url: Option<String>,
    format: Option<String>,
    max_length: Option<String>,
    query: Option<String>,
    count: Option<String>,
    width: Option<String>,
    height: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Fetch,
    Search,
    Screenshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Markdown,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchOptions {
    url: String,
    format: OutputFormat,
    max_length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchOptions {
    query: String,
    count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenshotOptions {
    url: String,
    viewport: Viewport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserError {
    InvalidInput(String),
    MissingConfig(String),
    RequestFailed(String),
    ParseFailed(String),
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    results: Option<Vec<BraveResult>>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: Option<String>,
    url: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListContext {
    Ordered { next_index: usize },
    Unordered,
}

#[derive(Debug, Default)]
struct ListState {
    stack: Vec<ListContext>,
}

impl ListState {
    fn open_list(&mut self, ordered: bool) {
        let context = if ordered {
            ListContext::Ordered { next_index: 1 }
        } else {
            ListContext::Unordered
        };
        self.stack.push(context);
    }

    fn close_list(&mut self, ordered: bool) {
        let index = self
            .stack
            .iter()
            .rposition(|context| list_context_matches(*context, ordered));
        if let Some(index) = index {
            self.stack.remove(index);
        }
    }

    fn list_item_prefix(&mut self) -> String {
        match self.stack.last_mut() {
            Some(ListContext::Ordered { next_index }) => {
                let prefix = format!("{next_index}. ");
                *next_index += 1;
                prefix
            }
            _ => "- ".to_string(),
        }
    }
}

fn list_context_matches(context: ListContext, ordered: bool) -> bool {
    matches!(
        (ordered, context),
        (true, ListContext::Ordered { .. }) | (false, ListContext::Unordered)
    )
}

#[derive(Serialize)]
struct FetchOutput<'a> {
    status: &'a str,
    url: &'a str,
    format: &'a str,
    content: &'a str,
    content_length: usize,
    truncated: bool,
    message: String,
}

#[derive(Serialize)]
struct SearchOutput<'a> {
    status: &'a str,
    query: &'a str,
    count: usize,
    results: Vec<SearchResult>,
    message: String,
}

#[derive(Serialize)]
struct ScreenshotOutput<'a> {
    status: &'a str,
    url: &'a str,
    width: u32,
    height: u32,
    image_base64: &'a str,
    format: &'a str,
    message: String,
}

struct HttpRequest<'a> {
    method: &'a str,
    url: &'a str,
    headers: &'a str,
    body: &'a str,
}

trait HostBridge {
    fn kv_get(&self, key: &str) -> Option<String>;
    fn http_request(&self, request: &HttpRequest<'_>) -> Option<String>;
}

struct LiveHostBridge;

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message)
            | Self::MissingConfig(message)
            | Self::RequestFailed(message)
            | Self::ParseFailed(message) => formatter.write_str(message),
        }
    }
}

impl Tool {
    fn parse(value: Option<&str>) -> Result<Self, BrowserError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            Some("web_fetch") => Ok(Self::Fetch),
            Some("web_search") => Ok(Self::Search),
            Some("web_screenshot") => Ok(Self::Screenshot),
            Some(value) => Err(BrowserError::InvalidInput(format!(
                "Unknown tool: {value}. Available: web_fetch, web_search, web_screenshot"
            ))),
            None => Err(BrowserError::InvalidInput(
                "Tool is required. Available: web_fetch, web_search, web_screenshot".to_string(),
            )),
        }
    }
}

impl OutputFormat {
    fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("text") => Self::Text,
            _ => Self::Markdown,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
        }
    }
}

impl HostBridge for LiveHostBridge {
    fn kv_get(&self, key: &str) -> Option<String> {
        unsafe { read_host_string(host_kv_get(key.as_ptr(), key.len() as u32)) }
    }

    fn http_request(&self, request: &HttpRequest<'_>) -> Option<String> {
        unsafe {
            read_host_string(host_http_request(
                request.method.as_ptr(),
                request.method.len() as u32,
                request.url.as_ptr(),
                request.url.len() as u32,
                request.headers.as_ptr(),
                request.headers.len() as u32,
                request.body.as_ptr(),
                request.body.len() as u32,
            ))
        }
    }
}

/// # Safety
/// `ptr` must be 0 or point to a NUL-terminated string in valid WASM linear memory.
unsafe fn read_host_string(ptr: u32) -> Option<String> {
    if ptr == 0 {
        return None;
    }

    let slice = core::slice::from_raw_parts(ptr as *const u8, MAX_HOST_STRING_LEN);
    let len = slice
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(MAX_HOST_STRING_LEN);
    Some(String::from_utf8_lossy(&slice[..len]).into_owned())
}

fn log(level: u32, message: &str) {
    unsafe {
        host_log(level, message.as_ptr(), message.len() as u32);
    }
}

fn get_input() -> String {
    unsafe { read_host_string(host_get_input()).unwrap_or_default() }
}

fn set_output(text: &str) {
    unsafe {
        host_set_output(text.as_ptr(), text.len() as u32);
    }
}

fn execute(raw_input: &str) -> Result<String, BrowserError> {
    let host = LiveHostBridge;
    execute_with_host(raw_input, &host)
}

fn execute_with_host(raw_input: &str, host: &impl HostBridge) -> Result<String, BrowserError> {
    let input = parse_input(raw_input)?;
    match Tool::parse(input.tool.as_deref())? {
        Tool::Fetch => execute_fetch(&input, host),
        Tool::Search => execute_search(&input, host),
        Tool::Screenshot => execute_screenshot(&input, host),
    }
}

fn parse_input(raw_input: &str) -> Result<BrowserInput, BrowserError> {
    serde_json::from_str(raw_input)
        .map_err(|error| BrowserError::InvalidInput(format!("Invalid input JSON: {error}")))
}

fn execute_fetch(input: &BrowserInput, host: &impl HostBridge) -> Result<String, BrowserError> {
    let options = parse_fetch_options(input)?;
    let html = http_get(host, &options.url, EMPTY_JSON)?;
    let extracted = extract_content(&html, options.format);
    let (content, truncated) = truncate_content(&extracted, options.max_length);
    Ok(serialize_json(&FetchOutput {
        status: "success",
        url: &options.url,
        format: options.format.as_str(),
        content: &content,
        content_length: content.chars().count(),
        truncated,
        message: format!(
            "📄 Fetched {} ({} chars)",
            options.url,
            comma_number(content.chars().count())
        ),
    }))
}

fn parse_fetch_options(input: &BrowserInput) -> Result<FetchOptions, BrowserError> {
    Ok(FetchOptions {
        url: require_url(input.url.as_deref())?,
        format: OutputFormat::parse(input.format.as_deref()),
        max_length: parse_positive_usize(input.max_length.as_deref(), DEFAULT_MAX_LENGTH),
    })
}

fn execute_search(input: &BrowserInput, host: &impl HostBridge) -> Result<String, BrowserError> {
    let options = parse_search_options(input)?;
    let api_key = require_stored_value(
        host.kv_get("brave_api_key"),
        "No Brave API key found. Set 'brave_api_key' in skill storage.",
    )?;
    let response = http_get(host, &build_search_url(&options), &search_headers(&api_key))?;
    let results = parse_search_results(&response)?;
    let count = results.len();
    Ok(serialize_json(&SearchOutput {
        status: "success",
        query: &options.query,
        count,
        results,
        message: format!("🔍 Found {count} results for: {}", options.query),
    }))
}

fn parse_search_options(input: &BrowserInput) -> Result<SearchOptions, BrowserError> {
    let query = input.query.clone().unwrap_or_default();
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(BrowserError::InvalidInput(
            "Search query is required".to_string(),
        ));
    }

    Ok(SearchOptions {
        query,
        count: clamp_count(input.count.as_deref()),
    })
}

fn execute_screenshot(
    input: &BrowserInput,
    host: &impl HostBridge,
) -> Result<String, BrowserError> {
    let options = parse_screenshot_options(input)?;
    let service_url = require_stored_value(
        host.kv_get("screenshot_service_url"),
        "Screenshot requires a screenshot service. Set 'screenshot_service_url' in skill storage (e.g., a self-hosted url-to-png service).",
    )?;
    let request_url = build_screenshot_url(&service_url, &options);
    let response = http_get(host, &request_url, EMPTY_JSON)?;
    let image_base64 = response_binary_base64(&response);
    Ok(serialize_json(&ScreenshotOutput {
        status: "success",
        url: &options.url,
        width: options.viewport.width,
        height: options.viewport.height,
        image_base64: &image_base64,
        format: "png",
        message: format!(
            "📸 Screenshot of {} ({}x{})",
            options.url, options.viewport.width, options.viewport.height
        ),
    }))
}

fn parse_screenshot_options(input: &BrowserInput) -> Result<ScreenshotOptions, BrowserError> {
    Ok(ScreenshotOptions {
        url: require_url(input.url.as_deref())?,
        viewport: Viewport {
            width: parse_positive_u32(input.width.as_deref(), DEFAULT_WIDTH),
            height: parse_positive_u32(input.height.as_deref(), DEFAULT_HEIGHT),
        },
    })
}

fn require_url(value: Option<&str>) -> Result<String, BrowserError> {
    let url = value.unwrap_or_default().trim().to_string();
    if url.is_empty() {
        return Err(BrowserError::InvalidInput("URL is required".to_string()));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(BrowserError::InvalidInput(
            "URL must start with http:// or https://".to_string(),
        ));
    }
    Ok(url)
}

fn parse_positive_usize(value: Option<&str>, default: usize) -> usize {
    value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(default)
}

fn parse_positive_u32(value: Option<&str>, default: u32) -> u32 {
    value
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(default)
}

fn clamp_count(value: Option<&str>) -> usize {
    value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SEARCH_COUNT)
        .clamp(1, MAX_SEARCH_COUNT)
}

fn require_stored_value(value: Option<String>, message: &str) -> Result<String, BrowserError> {
    value
        .map(|stored| stored.trim().to_string())
        .filter(|stored| !stored.is_empty())
        .ok_or_else(|| BrowserError::MissingConfig(message.to_string()))
}

fn http_get(host: &impl HostBridge, url: &str, headers: &str) -> Result<String, BrowserError> {
    let request = HttpRequest {
        method: "GET",
        url,
        headers,
        body: "",
    };
    host.http_request(&request).ok_or_else(|| {
        BrowserError::RequestFailed(format!("Failed to fetch {url}: request failed"))
    })
}

fn build_search_url(options: &SearchOptions) -> String {
    format!(
        "{BRAVE_SEARCH_URL}?q={}&count={}",
        encode_url_component(&options.query),
        options.count
    )
}

fn search_headers(api_key: &str) -> String {
    json!({
        "Accept": "application/json",
        "X-Subscription-Token": api_key
    })
    .to_string()
}

fn build_screenshot_url(service_url: &str, options: &ScreenshotOptions) -> String {
    let separator = if service_url.contains('?') { '&' } else { '?' };
    format!(
        "{service_url}{separator}url={}&width={}&height={}",
        encode_url_component(&options.url),
        options.viewport.width,
        options.viewport.height
    )
}

fn parse_search_results(response: &str) -> Result<Vec<SearchResult>, BrowserError> {
    if let Some(message) = extract_api_error(response) {
        return Err(BrowserError::RequestFailed(format!(
            "Failed to fetch {BRAVE_SEARCH_URL}: {message}"
        )));
    }

    let parsed: BraveResponse = serde_json::from_str(response).map_err(|error| {
        BrowserError::ParseFailed(format!("Failed to parse Brave search response: {error}"))
    })?;

    Ok(parsed
        .web
        .and_then(|web| web.results)
        .unwrap_or_default()
        .into_iter()
        .map(normalize_search_result)
        .collect())
}

fn normalize_search_result(result: BraveResult) -> SearchResult {
    SearchResult {
        title: result.title.unwrap_or_default(),
        url: result.url.unwrap_or_default(),
        snippet: result.description.unwrap_or_default(),
    }
}

fn extract_api_error(response: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(response).ok()?;
    value.get("error").and_then(json_error_message).or_else(|| {
        value
            .get("message")
            .and_then(|value| value.as_str())
            .map(str::to_string)
    })
}

fn json_error_message(value: &serde_json::Value) -> Option<String> {
    value
        .get("message")
        .and_then(|message| message.as_str())
        .map(str::to_string)
        .or_else(|| value.as_str().map(str::to_string))
}

fn extract_content(html: &str, format: OutputFormat) -> String {
    let without_blocks = strip_ignored_blocks(html);
    let with_pre = convert_pre_tags(&without_blocks, format);
    let with_code = convert_code_tags(&with_pre, format);
    let with_links = convert_links(&with_code, format);
    let with_headings = replace_heading_tags(&with_links, format);
    let with_blocks = replace_block_tags(&with_headings);
    let stripped = strip_tags(&with_blocks);
    let decoded = decode_html_entities(&stripped);
    normalize_text(&decoded)
}

fn strip_ignored_blocks(input: &str) -> String {
    let mut stripped = strip_html_comments(input);
    for tag in [
        "script", "style", "nav", "header", "footer", "noscript", "svg", "iframe", "form", "aside",
    ] {
        stripped = strip_tag_block(&stripped, tag);
    }
    stripped
}

fn strip_html_comments(input: &str) -> String {
    let mut result = String::new();
    let mut cursor = 0;

    while let Some(start_offset) = input[cursor..].find("<!--") {
        let start = cursor + start_offset;
        result.push_str(&input[cursor..start]);
        let Some(end_offset) = input[start + 4..].find("-->") else {
            return result;
        };
        cursor = start + 4 + end_offset + 3;
    }

    result.push_str(&input[cursor..]);
    result
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let mut result = String::new();
    let mut cursor = 0;

    while let Some((start, open_end)) = find_opening_tag(input, tag, cursor) {
        result.push_str(&input[cursor..start]);
        let Some((_, close_end)) = find_closing_tag(input, tag, open_end + 1) else {
            cursor = input.len();
            break;
        };
        cursor = close_end + 1;
    }

    result.push_str(&input[cursor..]);
    result
}

fn convert_pre_tags(input: &str, format: OutputFormat) -> String {
    replace_paired_tag(input, "pre", |inner| render_preformatted(inner, format))
}

fn convert_code_tags(input: &str, format: OutputFormat) -> String {
    replace_paired_tag(input, "code", |inner| render_inline_code(inner, format))
}

fn replace_paired_tag<F>(input: &str, tag: &str, render: F) -> String
where
    F: Fn(&str) -> String,
{
    let mut result = String::new();
    let mut cursor = 0;

    while let Some((start, open_end)) = find_opening_tag(input, tag, cursor) {
        result.push_str(&input[cursor..start]);
        let Some((close_start, close_end)) = find_closing_tag(input, tag, open_end + 1) else {
            result.push_str(&input[start..]);
            return result;
        };
        result.push_str(&render(&input[open_end + 1..close_start]));
        cursor = close_end + 1;
    }

    result.push_str(&input[cursor..]);
    result
}

fn render_preformatted(inner: &str, format: OutputFormat) -> String {
    let text = extract_inline_text(inner);
    if text.is_empty() {
        return String::new();
    }
    match format {
        OutputFormat::Markdown => format!("\n```\n{text}\n```\n"),
        OutputFormat::Text => format!("\n{text}\n"),
    }
}

fn render_inline_code(inner: &str, format: OutputFormat) -> String {
    let text = extract_inline_text(inner);
    if text.is_empty() {
        return String::new();
    }
    match format {
        OutputFormat::Markdown => format!("`{text}`"),
        OutputFormat::Text => text,
    }
}

fn convert_links(input: &str, format: OutputFormat) -> String {
    let mut result = String::new();
    let mut cursor = 0;

    while let Some((start, open_end)) = find_opening_tag(input, "a", cursor) {
        result.push_str(&input[cursor..start]);
        let Some((close_start, close_end)) = find_closing_tag(input, "a", open_end + 1) else {
            result.push_str(&input[start..]);
            return result;
        };
        let tag = &input[start..=open_end];
        let text = extract_inline_text(&input[open_end + 1..close_start]);
        let href = attribute_value(tag, "href");
        result.push_str(&render_link(&text, href.as_deref(), format));
        cursor = close_end + 1;
    }

    result.push_str(&input[cursor..]);
    result
}

fn render_link(text: &str, href: Option<&str>, format: OutputFormat) -> String {
    if text.is_empty() {
        return String::new();
    }
    match (format, href.filter(|value| !value.is_empty())) {
        (OutputFormat::Markdown, Some(url)) => format!("[{text}]({url})"),
        _ => text.to_string(),
    }
}

fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let lower = ascii_lowercase(tag);
    let bytes = lower.as_bytes();
    let target = name.as_bytes();

    for start in 0..=bytes.len().saturating_sub(target.len()) {
        if bytes[start..].starts_with(target) && is_attribute_start(bytes, start, target.len()) {
            return parse_attribute_value(tag, start + target.len());
        }
    }

    None
}

fn is_attribute_start(bytes: &[u8], start: usize, name_len: usize) -> bool {
    let before = start
        .checked_sub(1)
        .and_then(|index| bytes.get(index))
        .copied();
    let after = bytes.get(start + name_len).copied();
    is_attribute_boundary(before) && matches!(after, Some(b'=') | Some(b' ') | Some(b'\t'))
}

fn is_attribute_boundary(byte: Option<u8>) -> bool {
    matches!(
        byte,
        None | Some(b'<') | Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t')
    )
}

fn parse_attribute_value(tag: &str, start: usize) -> Option<String> {
    let rest = tag.get(start..)?.trim_start();
    let value = rest.strip_prefix('=')?.trim_start();
    let quote = value.chars().next()?;

    if matches!(quote, '"' | '\'') {
        let end = value.get(1..)?.find(quote)?;
        return value.get(1..1 + end).map(str::to_string);
    }

    let end = value
        .find(|character: char| character.is_whitespace() || character == '>')
        .unwrap_or(value.len());
    value.get(..end).map(str::to_string)
}

fn replace_heading_tags(input: &str, format: OutputFormat) -> String {
    let h1 = replace_heading_level(input, 1, format);
    let h2 = replace_heading_level(&h1, 2, format);
    let h3 = replace_heading_level(&h2, 3, format);
    let h4 = replace_heading_level(&h3, 4, format);
    let h5 = replace_heading_level(&h4, 5, format);
    replace_heading_level(&h5, 6, format)
}

fn replace_heading_level(input: &str, level: usize, format: OutputFormat) -> String {
    let tag = format!("h{level}");
    let prefix = match format {
        OutputFormat::Markdown => format!("\n{} ", "#".repeat(level)),
        OutputFormat::Text => "\n".to_string(),
    };
    let opened = replace_tag(input, &tag, false, &prefix);
    replace_tag(&opened, &tag, true, "\n\n")
}

fn replace_block_tags(input: &str) -> String {
    let mut result = String::new();
    let mut state = ListState::default();
    let mut cursor = 0;

    while let Some((start, end)) = next_tag_bounds(input, cursor) {
        result.push_str(&input[cursor..start]);
        result.push_str(&render_block_tag(&input[start..=end], &mut state));
        cursor = end + 1;
    }

    result.push_str(&input[cursor..]);
    result
}

fn next_tag_bounds(input: &str, offset: usize) -> Option<(usize, usize)> {
    let start = input.get(offset..)?.find('<')? + offset;
    let end = input.get(start..)?.find('>')? + start;
    Some((start, end))
}

fn render_block_tag(tag: &str, state: &mut ListState) -> String {
    match tag_name(tag) {
        Some((true, name)) if name == "p" => "\n\n".to_string(),
        Some((false, name)) if name == "br" => "\n".to_string(),
        Some((false, name)) if name == "ol" => {
            state.open_list(true);
            String::new()
        }
        Some((true, name)) if name == "ol" => {
            state.close_list(true);
            String::new()
        }
        Some((false, name)) if name == "ul" => {
            state.open_list(false);
            String::new()
        }
        Some((true, name)) if name == "ul" => {
            state.close_list(false);
            String::new()
        }
        Some((false, name)) if name == "li" => state.list_item_prefix(),
        Some((true, name)) if name == "li" => "\n".to_string(),
        _ => String::new(),
    }
}

fn tag_name(tag: &str) -> Option<(bool, String)> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    let closing = inner.starts_with('/');
    let inner = inner.strip_prefix('/').unwrap_or(inner).trim();
    let inner = inner.strip_suffix('/').unwrap_or(inner).trim();
    let name = inner.split_whitespace().next()?;
    Some((closing, name.to_ascii_lowercase()))
}

fn replace_tag(input: &str, tag: &str, closing: bool, replacement: &str) -> String {
    let mut result = String::new();
    let mut cursor = 0;

    while let Some((start, end)) = find_tag(input, tag, closing, cursor) {
        result.push_str(&input[cursor..start]);
        result.push_str(replacement);
        cursor = end + 1;
    }

    result.push_str(&input[cursor..]);
    result
}

fn find_tag(input: &str, tag: &str, closing: bool, offset: usize) -> Option<(usize, usize)> {
    if closing {
        return find_closing_tag(input, tag, offset);
    }
    find_opening_tag(input, tag, offset)
}

fn find_opening_tag(input: &str, tag: &str, offset: usize) -> Option<(usize, usize)> {
    find_tag_bounds(input, &format!("<{tag}"), offset)
}

fn find_closing_tag(input: &str, tag: &str, offset: usize) -> Option<(usize, usize)> {
    find_tag_bounds(input, &format!("</{tag}"), offset)
}

fn find_tag_bounds(input: &str, needle: &str, offset: usize) -> Option<(usize, usize)> {
    let lower = ascii_lowercase(input);
    let bytes = lower.as_bytes();
    let mut cursor = offset;

    while cursor < lower.len() {
        let index = lower.get(cursor..)?.find(needle)? + cursor;
        let boundary = bytes.get(index + needle.len()).copied();
        if is_tag_boundary(boundary) {
            let end = lower.get(index..)?.find('>')? + index;
            return Some((index, end));
        }
        cursor = index + needle.len();
    }

    None
}

fn is_tag_boundary(byte: Option<u8>) -> bool {
    matches!(
        byte,
        None | Some(b'>') | Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t') | Some(b'/')
    )
}

fn strip_tags(input: &str) -> String {
    let mut result = String::new();
    let mut inside_tag = false;

    for character in input.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => result.push(character),
            _ => {}
        }
    }

    result
}

fn extract_inline_text(input: &str) -> String {
    let stripped = strip_tags(input);
    let decoded = decode_html_entities(&stripped);
    normalize_text(&decoded)
}

fn decode_html_entities(input: &str) -> String {
    let mut result = String::new();
    let mut cursor = 0;

    while let Some(start_offset) = input[cursor..].find('&') {
        let start = cursor + start_offset;
        result.push_str(&input[cursor..start]);
        let Some(end_offset) = input[start..].find(';') else {
            result.push_str(&input[start..]);
            return result;
        };
        let end = start + end_offset;
        let entity = &input[start + 1..end];
        match decode_html_entity(entity) {
            Some(character) => result.push(character),
            None => result.push_str(&input[start..=end]),
        }
        cursor = end + 1;
    }

    result.push_str(&input[cursor..]);
    result
}

fn decode_html_entity(entity: &str) -> Option<char> {
    match entity {
        "nbsp" => Some(' '),
        "#39" | "apos" => Some('\''),
        "quot" => Some('"'),
        "gt" => Some('>'),
        "lt" => Some('<'),
        "amp" => Some('&'),
        _ => decode_numeric_entity(entity),
    }
}

fn decode_numeric_entity(entity: &str) -> Option<char> {
    let (digits, radix) = match entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        Some(hex) => (hex, 16),
        None => (entity.strip_prefix('#')?, 10),
    };
    let value = u32::from_str_radix(digits, radix).ok()?;
    char::from_u32(value)
}

fn normalize_text(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    for line in normalized.lines() {
        lines.push(normalize_line(line));
    }
    collapse_blank_lines(&lines)
}

fn normalize_line(line: &str) -> String {
    let mut normalized = String::new();
    let mut previous_space = false;

    for character in line.trim().chars() {
        if character.is_whitespace() {
            if !previous_space {
                normalized.push(' ');
            }
            previous_space = true;
            continue;
        }
        normalized.push(character);
        previous_space = false;
    }

    normalized
}

fn collapse_blank_lines(lines: &[String]) -> String {
    let mut kept = Vec::new();
    let mut previous_blank = false;

    for line in lines {
        if line.is_empty() {
            if !previous_blank {
                kept.push(String::new());
            }
            previous_blank = true;
            continue;
        }
        kept.push(line.clone());
        previous_blank = false;
    }

    kept.join("\n").trim().to_string()
}

fn truncate_content(content: &str, max_length: usize) -> (String, bool) {
    let total = content.chars().count();
    if total <= max_length {
        return (content.to_string(), false);
    }
    (content.chars().take(max_length).collect(), true)
}

fn response_binary_base64(response: &str) -> String {
    match response.strip_prefix(HOST_BINARY_BASE64_PREFIX) {
        Some(encoded) => encoded.to_string(),
        None => base64_encode(response.as_bytes()),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let combined = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;

        output.push(TABLE[((combined >> 18) & 0x3F) as usize] as char);
        output.push(TABLE[((combined >> 12) & 0x3F) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((combined >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(combined & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn encode_url_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn ascii_lowercase(value: &str) -> String {
    value.to_ascii_lowercase()
}

fn comma_number(value: usize) -> String {
    let mut chars: Vec<char> = value.to_string().chars().rev().collect();
    let mut index = 3;
    while index < chars.len() {
        chars.insert(index, ',');
        index += 4;
    }
    chars.into_iter().rev().collect()
}

fn error_output(error: &BrowserError) -> String {
    serialize_json(&json!({ "error": error.to_string() }))
}

fn serialize_json<T: Serialize>(value: &T) -> String {
    match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(_) => r#"{"error":"Internal serialization error."}"#.to_string(),
    }
}

#[no_mangle]
pub extern "C" fn run() {
    log(INFO_LEVEL, "Browser skill starting");
    let input = get_input();

    match execute(&input) {
        Ok(output) => set_output(&output),
        Err(error) => {
            log(ERROR_LEVEL, &error.to_string());
            set_output(&error_output(&error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeHost {
        storage: HashMap<String, String>,
        responses: HashMap<String, String>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                storage: HashMap::new(),
                responses: HashMap::new(),
            }
        }

        fn with_storage(mut self, key: &str, value: &str) -> Self {
            self.storage.insert(key.to_string(), value.to_string());
            self
        }

        fn with_response(mut self, url: &str, body: &str) -> Self {
            self.responses.insert(url.to_string(), body.to_string());
            self
        }
    }

    impl HostBridge for FakeHost {
        fn kv_get(&self, key: &str) -> Option<String> {
            self.storage.get(key).cloned()
        }

        fn http_request(&self, request: &HttpRequest<'_>) -> Option<String> {
            self.responses.get(request.url).cloned()
        }
    }

    #[test]
    fn tool_routing_executes_web_fetch() {
        let host = FakeHost::new().with_response("https://example.com", "<h1>Example</h1>");
        let output =
            execute_with_host(r#"{"tool":"web_fetch","url":"https://example.com"}"#, &host)
                .expect("fetch should succeed");
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("json");

        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["format"], "markdown");
        assert_eq!(parsed["content"], "# Example");
    }

    #[test]
    fn tool_routing_executes_web_search() {
        let body = r#"{"web":{"results":[{"title":"Rust","url":"https://www.rust-lang.org","description":"Fast systems language"}]}}"#;
        let host = FakeHost::new()
            .with_storage("brave_api_key", "secret")
            .with_response(
                "https://api.search.brave.com/res/v1/web/search?q=rust%20async&count=1",
                body,
            );
        let output = execute_with_host(
            r#"{"tool":"web_search","query":"rust async","count":"1"}"#,
            &host,
        )
        .expect("search should succeed");
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("json");

        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["results"][0]["title"], "Rust");
    }

    #[test]
    fn tool_routing_executes_web_screenshot() {
        let response = format!("{HOST_BINARY_BASE64_PREFIX}AQID");
        let host = FakeHost::new()
            .with_storage("screenshot_service_url", "https://shots.example/render")
            .with_response(
                "https://shots.example/render?url=https%3A%2F%2Fexample.com&width=1280&height=720",
                &response,
            );
        let output = execute_with_host(
            r#"{"tool":"web_screenshot","url":"https://example.com"}"#,
            &host,
        )
        .expect("screenshot should succeed");
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("json");

        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["image_base64"], "AQID");
        assert_eq!(parsed["width"], 1280);
        assert_eq!(parsed["height"], 720);
    }

    #[test]
    fn tool_routing_rejects_unknown_tool() {
        let error = execute_with_host(r#"{"tool":"wat"}"#, &FakeHost::new())
            .expect_err("unknown tool should fail");
        assert_eq!(
            error.to_string(),
            "Unknown tool: wat. Available: web_fetch, web_search, web_screenshot"
        );
    }

    #[test]
    fn web_fetch_requires_url() {
        let error = parse_fetch_options(&input_with_tool("web_fetch")).expect_err("url required");
        assert_eq!(error.to_string(), "URL is required");
    }

    #[test]
    fn web_fetch_rejects_invalid_url_scheme() {
        let error = require_url(Some("ftp://example.com")).expect_err("scheme should fail");
        assert_eq!(error.to_string(), "URL must start with http:// or https://");
    }

    #[test]
    fn web_fetch_converts_html_to_markdown() {
        let html = concat!(
            "<header>ignore me</header>",
            "<h1>Example Domain</h1>",
            "<p>This domain is for <a href=\"https://example.com/docs\">documentation</a>.</p>",
            "<ul><li>One</li><li>Two</li></ul>",
            "<pre>let value = 1;</pre>",
            "<script>alert('nope')</script>"
        );
        let content = extract_content(html, OutputFormat::Markdown);

        assert_eq!(
            content,
            "# Example Domain\n\nThis domain is for [documentation](https://example.com/docs).\n\n- One\n- Two\n\n```\nlet value = 1;\n```"
        );
    }

    #[test]
    fn web_fetch_text_format_skips_markdown_markup() {
        let html = "<h2>Title</h2><p>Hello <a href=\"https://example.com\">world</a><br>again</p><code>x</code>";
        let content = extract_content(html, OutputFormat::Text);
        assert_eq!(content, "Title\n\nHello world\nagain\n\nx");
    }

    #[test]
    fn web_fetch_truncates_content_at_max_length() {
        let (content, truncated) = truncate_content("abcdef", 4);
        assert_eq!(content, "abcd");
        assert!(truncated);
    }

    #[test]
    fn web_fetch_handles_multibyte_html_without_shifting_tag_offsets() {
        let html = "前缀🙂 <p>Visit <a href=\"https://example.com\">世界</a></p>";
        let content = extract_content(html, OutputFormat::Markdown);

        assert_eq!(content, "前缀🙂 Visit [世界](https://example.com)");
    }

    #[test]
    fn attribute_value_supports_non_href_names() {
        assert_eq!(
            attribute_value("<img src=\"hero.png\" class=\"cover\">", "src"),
            Some("hero.png".to_string())
        );
    }

    #[test]
    fn web_fetch_strips_comments_and_additional_noise_tags() {
        let html = concat!(
            "<!-- ignore me -->",
            "<aside>sidebar</aside>",
            "<noscript>fallback</noscript>",
            "<svg><text>icon</text></svg>",
            "<iframe>embedded</iframe>",
            "<form>controls</form>",
            "<p>Visible</p>"
        );
        let content = extract_content(html, OutputFormat::Text);

        assert_eq!(content, "Visible");
    }

    #[test]
    fn web_fetch_decodes_named_and_numeric_entities() {
        let content = extract_content(
            "<p>Tom &amp; Jerry &lt;3 &quot;cartoons&quot; &#39;always&#39; &apos;yes&apos; &#169; &#x1F680; &nbsp;!</p>",
            OutputFormat::Text,
        );
        assert_eq!(content, "Tom & Jerry <3 \"cartoons\" 'always' 'yes' © 🚀 !");
    }

    #[test]
    fn web_fetch_preserves_ordered_list_numbers() {
        let html = "<ol><li>First</li><li>Second</li></ol><ul><li>Third</li></ul>";
        let content = extract_content(html, OutputFormat::Text);

        assert_eq!(content, "1. First\n2. Second\n- Third");
    }

    #[test]
    fn web_search_requires_query() {
        let error = parse_search_options(&BrowserInput {
            tool: Some("web_search".to_string()),
            url: None,
            format: None,
            max_length: None,
            query: None,
            count: None,
            width: None,
            height: None,
        })
        .expect_err("query required");
        assert_eq!(error.to_string(), "Search query is required");
    }

    #[test]
    fn web_search_clamps_count() {
        assert_eq!(clamp_count(Some("0")), 1);
        assert_eq!(clamp_count(Some("99")), 10);
        assert_eq!(clamp_count(Some("bogus")), 5);
    }

    #[test]
    fn web_search_requires_api_key() {
        let error = execute_search(
            &BrowserInput {
                tool: Some("web_search".to_string()),
                url: None,
                format: None,
                max_length: None,
                query: Some("rust".to_string()),
                count: None,
                width: None,
                height: None,
            },
            &FakeHost::new(),
        )
        .expect_err("missing api key should fail");
        assert_eq!(
            error.to_string(),
            "No Brave API key found. Set 'brave_api_key' in skill storage."
        );
    }

    #[test]
    fn web_search_parses_results() {
        let results = parse_search_results(
            r#"{"web":{"results":[{"title":"Rust","url":"https://www.rust-lang.org","description":"Systems language"}]}}"#,
        )
        .expect("results should parse");
        assert_eq!(
            results,
            vec![SearchResult {
                title: "Rust".to_string(),
                url: "https://www.rust-lang.org".to_string(),
                snippet: "Systems language".to_string(),
            }]
        );
    }

    #[test]
    fn web_search_surfaces_error_payloads() {
        let error = parse_search_results(r#"{"error":{"message":"rate limited"}}"#)
            .expect_err("error payload should fail");
        assert_eq!(
            error.to_string(),
            "Failed to fetch https://api.search.brave.com/res/v1/web/search: rate limited"
        );
    }

    #[test]
    fn web_screenshot_requires_url() {
        let error =
            parse_screenshot_options(&input_with_tool("web_screenshot")).expect_err("url required");
        assert_eq!(error.to_string(), "URL is required");
    }

    #[test]
    fn web_screenshot_requires_service_url() {
        let error = execute_screenshot(
            &BrowserInput {
                tool: Some("web_screenshot".to_string()),
                url: Some("https://example.com".to_string()),
                format: None,
                max_length: None,
                query: None,
                count: None,
                width: None,
                height: None,
            },
            &FakeHost::new(),
        )
        .expect_err("service url required");
        assert_eq!(
            error.to_string(),
            "Screenshot requires a screenshot service. Set 'screenshot_service_url' in skill storage (e.g., a self-hosted url-to-png service)."
        );
    }

    #[test]
    fn web_screenshot_defaults_dimensions() {
        let options = parse_screenshot_options(&BrowserInput {
            tool: Some("web_screenshot".to_string()),
            url: Some("https://example.com".to_string()),
            format: None,
            max_length: None,
            query: None,
            count: None,
            width: None,
            height: None,
        })
        .expect("options should parse");
        assert_eq!(options.viewport.width, 1280);
        assert_eq!(options.viewport.height, 720);
    }

    #[test]
    fn url_encoding_handles_special_characters() {
        assert_eq!(
            encode_url_component("São Paulo & a+b=?#%"),
            "S%C3%A3o%20Paulo%20%26%20a%2Bb%3D%3F%23%25"
        );
        assert_eq!(
            encode_url_component("https://example.com/a:b"),
            "https%3A%2F%2Fexample.com%2Fa%3Ab"
        );
    }

    #[test]
    fn response_binary_base64_preserves_host_encoded_payloads() {
        let response = format!("{HOST_BINARY_BASE64_PREFIX}Zm9v");
        assert_eq!(response_binary_base64(&response), "Zm9v");
    }

    #[test]
    fn http_failures_include_requested_url() {
        let error = http_get(&FakeHost::new(), "https://example.com", EMPTY_JSON)
            .expect_err("request should fail");
        assert_eq!(
            error.to_string(),
            "Failed to fetch https://example.com: request failed"
        );
    }

    #[test]
    fn manifest_declares_expected_tools_and_capabilities() {
        let manifest = include_str!("../manifest.toml");
        assert!(manifest.contains(r#"capabilities = ["network", "storage"]"#));
        assert!(manifest.contains(r#"name = "web_fetch""#));
        assert!(manifest.contains(r#"name = "web_search""#));
        assert!(manifest.contains(r#"name = "web_screenshot""#));
    }

    fn input_with_tool(tool: &str) -> BrowserInput {
        BrowserInput {
            tool: Some(tool.to_string()),
            url: None,
            format: None,
            max_length: None,
            query: None,
            count: None,
            width: None,
            height: None,
        }
    }
}
