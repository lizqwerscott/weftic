use std::{collections::HashMap, fs};

use anyhow::{Result, anyhow};
use minijinja::{Environment, context};

use crate::config::system_prompt::SystemPromptConfig;

pub struct SystemPrompt {
    prompts: HashMap<String, String>,
}

impl SystemPrompt {
    pub fn build(config: &SystemPromptConfig, tools: &[String]) -> Result<Self> {
        let path = config.character_path.as_path();

        if !path.exists() {
            return Err(anyhow!(
                "character card path {} does not exist",
                path.display()
            ));
        }

        let character = fs::read_to_string(path)?;

        let mut prompts = HashMap::new();

        let tool_description = tools.join("\n\n");

        let agent_prompt = fs::read_to_string("./prompts/agent.j2")?;
        let mut env = Environment::new();
        env.set_trim_blocks(true);

        env.add_template("agent", &agent_prompt)?;

        let agent_template = env.get_template("agent")?;
        let rendered = agent_template
            .render(context!(character_card => character, tool_description => tool_description))?;

        prompts.insert("agent".to_string(), rendered);

        Ok(Self { prompts })
    }

    pub fn render_system_prompt(&self, name: &str) -> Option<String> {
        self.prompts.get(name).map(|prompt| prompt.to_string())
    }
}
