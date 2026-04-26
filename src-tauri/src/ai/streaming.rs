pub fn extract_visible_instruction(partial_json: &str) -> String {
    let prefix = "\"instruction\":";
    if let Some(idx) = partial_json.rfind(prefix) {
        let remainder = &partial_json[idx + prefix.len()..];
        let trimmed = remainder.trim_start();
        if trimmed.starts_with('"') {
            let mut result = String::new();
            let mut chars = trimmed[1..].chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    if let Some(next_ch) = chars.next() {
                        if next_ch == '"' { result.push('"'); }
                        else if next_ch == 'n' { result.push('\n'); }
                        else if next_ch == 't' { result.push('\t'); }
                        else { result.push(next_ch); }
                    }
                } else if ch == '"' {
                    break;
                } else {
                    result.push(ch);
                }
            }
            return result;
        }
    }
    String::new()
}
