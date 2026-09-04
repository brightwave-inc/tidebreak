//! Prompt-safe skill catalog rendering shared by foreground and sandbox runs.

use std::collections::BTreeSet;

use crate::{PluginPackage, SkillOrigin, SkillPackage};

/// Render the skill catalog with concise descriptions.
pub fn skill_catalog_lines(skills: &[SkillPackage], plugins: &[PluginPackage]) -> Vec<String> {
    skill_catalog_lines_with(skills, plugins, skill_line)
}

/// Render the skill catalog with each package's pinned install hints.
pub fn skill_summary_catalog_lines(
    skills: &[SkillPackage],
    plugins: &[PluginPackage],
) -> Vec<String> {
    skill_catalog_lines_with(skills, plugins, skill_summary_line)
}

fn skill_line(skill: &SkillPackage) -> Option<String> {
    if !crate::is_valid_skill_name(&skill.name)
        || !crate::is_valid_skill_description(&skill.description)
    {
        return None;
    }
    let attribution = match skill.origin {
        SkillOrigin::Builtin => "",
        SkillOrigin::User => " (yours)",
    };
    Some(format!(
        "- {}: {}{attribution}",
        skill.name, skill.description
    ))
}

fn skill_summary_line(skill: &SkillPackage) -> Option<String> {
    let base = skill_line(skill)?;
    let mut hints = Vec::new();
    if !skill.python_deps.is_empty() {
        hints.push(format!(
            "pip install --user {}",
            skill.python_deps.join(" ")
        ));
    }
    if !skill.npm_deps.is_empty() {
        hints.push(format!(
            "npm install --ignore-scripts {}",
            skill.npm_deps.join(" ")
        ));
    }
    if hints.is_empty() {
        Some(base)
    } else {
        Some(format!("{base} [{}]", hints.join("; ")))
    }
}

fn skill_catalog_lines_with(
    skills: &[SkillPackage],
    plugins: &[PluginPackage],
    mut render: impl FnMut(&SkillPackage) -> Option<String>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut grouped: BTreeSet<&str> = BTreeSet::new();
    for plugin in plugins {
        let members: Vec<&SkillPackage> = plugin
            .skills
            .iter()
            .filter_map(|member| skills.iter().find(|skill| skill.name == *member))
            .collect();
        let member_lines: Vec<String> = members.iter().copied().filter_map(&mut render).collect();
        if member_lines.is_empty() {
            continue;
        }
        if let Some(preamble) = plugin
            .router_preamble
            .as_deref()
            .filter(|preamble| crate::is_valid_plugin_router_preamble(preamble))
        {
            lines.push(format!("- {preamble}"));
        }
        grouped.extend(members.iter().map(|skill| skill.name.as_str()));
        lines.extend(member_lines);
    }
    lines.extend(
        skills
            .iter()
            .filter(|skill| !grouped.contains(skill.name.as_str()))
            .filter_map(render),
    );
    lines
}
