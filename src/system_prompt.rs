use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow};
use minijinja::{Environment, context};

use crate::config::system_prompt::SystemPromptConfig;

pub struct SystemPromptManager {
    env: Environment<'static>,
    character: String,
}

impl SystemPromptManager {
    pub fn new(
        config: &SystemPromptConfig,
        templates: impl IntoIterator<Item = String>,
    ) -> Result<Self> {
        let mut template_names: Vec<String> = templates.into_iter().collect();

        template_names.sort();
        template_names.dedup();

        let path = config.character_path.as_path();

        if !path.exists() {
            return Err(anyhow!(
                "character card path {} does not exist",
                path.display()
            ));
        }

        let character = fs::read_to_string(path)?;

        let prompt_dir_path = Path::new("./prompts");

        let mut env = Environment::new();
        env.set_trim_blocks(true);

        for template in template_names {
            if !is_valid_template_name(&template) {
                return Err(anyhow!(
                    "invalid prompt template name `{template}` (allowed: [A-Za-z0-9_-]+)"
                ));
            }

            let path = prompt_dir_path.join(Path::new(&format!("{}.j2", template)));

            let prompt = fs::read_to_string(&path)
                .with_context(|| format!("reading prompt template {}", path.display()))?;
            env.add_template_owned(template.clone(), prompt)
                .with_context(|| format!("compiling prompt template {}", path.display()))?;
        }

        Ok(Self { env, character })
    }

    pub fn render(&self, name: &str, tools: &[String]) -> Result<String> {
        let prompt_template = self.env.get_template(name)?;

        let tool_description = tools.join("\n\n");

        let prompt = prompt_template.render(
            context! {character_card => self.character, tool_description => tool_description},
        )?;

        Ok(prompt)
    }
}

fn is_valid_template_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_template_names_are_accepted() {
        assert!(is_valid_template_name("agent"));
        assert!(is_valid_template_name("telegram-chat_v2"));
    }

    #[test]
    fn path_separators_are_rejected() {
        assert!(!is_valid_template_name("../x"));
        assert!(!is_valid_template_name("a/b"));
    }

    #[test]
    fn empty_names_are_rejected() {
        assert!(!is_valid_template_name(""));
    }
}
