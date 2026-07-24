pub mod web_search;

use xai_grok_tools::registry::types::ToolRegistryBuilder;

pub fn register(b: &mut ToolRegistryBuilder) {
    b.register::<web_search::OmgbWebSearchTool>();
}
