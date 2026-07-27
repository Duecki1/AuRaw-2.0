use std::fs;
use std::path::Path;

pub const INCLUDE_DIRECTIVE: &str = "// @include ";
pub const GENERATED_SHADER_TEMPLATES: [(&str, &str); 2] = [
    ("pass4.wgsl", "pass4.generated.wgsl"),
    ("xtrans_pass7.wgsl", "xtrans_pass7.generated.wgsl"),
];

pub fn generate_shader_sources(shader_dir: &Path, output_dir: &Path) -> Result<(), String> {
    for (template, output) in GENERATED_SHADER_TEMPLATES {
        let template_path = shader_dir.join(template);
        let generated = preprocess_shader(&template_path)?;
        let output_path = output_dir.join(output);
        fs::write(&output_path, generated)
            .map_err(|error| format!("could not write {}: {error}", output_path.display()))?;
    }
    Ok(())
}

pub fn preprocess_shader(path: &Path) -> Result<String, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let shader_dir = path
        .parent()
        .ok_or_else(|| format!("shader template {} has no parent directory", path.display()))?;
    preprocess_shader_source(path, shader_dir, &source)
}

fn preprocess_shader_source(path: &Path, shader_dir: &Path, source: &str) -> Result<String, String> {
    let mut generated = String::with_capacity(source.len());

    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(argument) = trimmed.strip_prefix(INCLUDE_DIRECTIVE) {
            let include = parse_include_argument(path, trimmed, argument)?;
            let include_path = shader_dir.join(include);
            let fragment = fs::read_to_string(&include_path)
                .map_err(|error| format!("could not read {}: {error}", include_path.display()))?;
            generated.push_str("// BEGIN generated include: ");
            generated.push_str(include);
            generated.push('\n');
            generated.push_str(&fragment);
            if !fragment.ends_with('\n') {
                generated.push('\n');
            }
            generated.push_str("// END generated include\n");
        } else {
            generated.push_str(line);
            generated.push('\n');
        }
    }

    Ok(generated)
}

fn parse_include_argument<'a>(
    path: &Path,
    directive: &str,
    argument: &'a str,
) -> Result<&'a str, String> {
    let include = argument
        .trim()
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| {
            format!(
                "{} has an invalid shader include directive: {directive}",
                path.display()
            )
        })?;

    if include.is_empty()
        || include.contains('\\')
        || Path::new(include).file_name().and_then(|name| name.to_str()) != Some(include)
    {
        return Err(format!(
            "{} includes an invalid shader fragment path: {include:?}",
            path.display()
        ));
    }

    Ok(include)
}

#[cfg(test)]
mod tests {
    use super::{
        generate_shader_sources, preprocess_shader, GENERATED_SHADER_TEMPLATES, INCLUDE_DIRECTIVE,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "auraw-shader-preprocessor-{}-{name}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create shader-preprocessor fixture directory");
        root
    }

    #[test]
    fn generated_shader_mapping_is_explicit_and_stable() {
        assert_eq!(
            GENERATED_SHADER_TEMPLATES,
            [
                ("pass4.wgsl", "pass4.generated.wgsl"),
                ("xtrans_pass7.wgsl", "xtrans_pass7.generated.wgsl"),
            ]
        );
    }

    #[test]
    fn include_directive_expands_a_sibling_fragment_once() {
        let root = temporary_directory("include");
        let template = root.join("template.wgsl");
        fs::write(
            &template,
            format!(
                "fn before() {{}}\n{INCLUDE_DIRECTIVE}\"shared.wgsl\"\nfn after() {{}}\n"
            ),
        )
        .unwrap();
        fs::write(root.join("shared.wgsl"), "fn shared() {}\n").unwrap();

        let generated = preprocess_shader(&template).expect("preprocess shader template");
        assert_eq!(generated.matches("fn shared()").count(), 1);
        assert!(!generated.contains(INCLUDE_DIRECTIVE));
        assert!(generated.contains("// BEGIN generated include: shared.wgsl"));
        assert!(generated.contains("// END generated include"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn include_directive_rejects_paths_outside_the_shader_directory() {
        let root = temporary_directory("traversal");
        let template = root.join("template.wgsl");
        fs::write(
            &template,
            format!("{INCLUDE_DIRECTIVE}\"../outside.wgsl\"\n"),
        )
        .unwrap();

        let error = preprocess_shader(&template).expect_err("path traversal must be rejected");
        assert!(error.contains("invalid shader fragment path"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generator_writes_every_declared_output() {
        let root = temporary_directory("generate");
        let shader_dir = root.join("shaders");
        let output_dir = root.join("out");
        fs::create_dir_all(&shader_dir).unwrap();
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(shader_dir.join("shared.wgsl"), "fn shared() {}\n").unwrap();
        for (template, _) in GENERATED_SHADER_TEMPLATES {
            fs::write(
                shader_dir.join(template),
                format!("{INCLUDE_DIRECTIVE}\"shared.wgsl\"\n"),
            )
            .unwrap();
        }

        generate_shader_sources(&shader_dir, &output_dir).expect("generate shader outputs");
        for (_, output) in GENERATED_SHADER_TEMPLATES {
            let generated = fs::read_to_string(output_dir.join(output)).unwrap();
            assert_eq!(generated.matches("fn shared()").count(), 1);
        }

        fs::remove_dir_all(root).unwrap();
    }
}
