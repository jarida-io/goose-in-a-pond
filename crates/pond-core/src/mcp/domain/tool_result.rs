/// One tool's output from a multi-tool dispatch (`ToolAgent::process_multi()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// Name of the tool that produced this result (e.g. "weather", "schedules").
    pub tool_name: String,
    pub content: String,
}

impl ToolResult {
    pub fn new(tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_new() {
        let r = ToolResult::new("weather", "Sunny, 25C");
        assert_eq!(r.tool_name, "weather");
        assert_eq!(r.content, "Sunny, 25C");
    }

    #[test]
    fn tool_result_clone_and_eq() {
        let a = ToolResult::new("weather", "data");
        let b = a.clone();
        assert_eq!(a, b);
    }
}
