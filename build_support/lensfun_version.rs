use std::fmt;

pub const MIN_SUPPORTED: LensfunVersion = LensfunVersion::new(0, 3, 2);
pub const MAX_SUPPORTED: LensfunVersion = LensfunVersion::new(0, 3, 4);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LensfunVersion {
    pub major: u32,
    pub minor: u32,
    pub micro: u32,
}

impl LensfunVersion {
    pub const fn new(major: u32, minor: u32, micro: u32) -> Self {
        Self {
            major,
            minor,
            micro,
        }
    }
}

impl fmt::Display for LensfunVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.micro)
    }
}

pub fn parse_lensfun_version(value: &str) -> Result<LensfunVersion, String> {
    let numeric = value
        .split(['-', '+'])
        .next()
        .ok_or_else(|| format!("Lensfun version {value:?} is empty"))?;
    let mut components = numeric.split('.');
    let parse_component = |component: Option<&str>, label: &str| -> Result<u32, String> {
        component
            .ok_or_else(|| format!("Lensfun version {value:?} is missing its {label} component"))?
            .parse::<u32>()
            .map_err(|_| format!("Lensfun version {value:?} has an invalid {label} component"))
    };
    let version = LensfunVersion::new(
        parse_component(components.next(), "major")?,
        parse_component(components.next(), "minor")?,
        parse_component(components.next(), "micro")?,
    );
    for component in components {
        let extra = component.parse::<u32>().map_err(|_| {
            format!("Lensfun version {value:?} has an invalid trailing component")
        })?;
        if extra != 0 {
            return Err(format!(
                "Lensfun version {value:?} has an unsupported non-zero trailing component"
            ));
        }
    }
    Ok(version)
}

pub fn parse_lensfun_header_version(contents: &str) -> Result<LensfunVersion, String> {
    fn define(contents: &str, name: &str) -> Result<u32, String> {
        contents
            .lines()
            .find_map(|line| {
                let mut tokens = line.split_whitespace();
                match (tokens.next(), tokens.next(), tokens.next()) {
                    (Some("#define"), Some(found), Some(value)) if found == name => {
                        Some(
                            value
                                .trim_matches(|character| character == '(' || character == ')')
                                .parse::<u32>()
                                .map_err(|_| {
                                    format!(
                                        "Lensfun header has an invalid {name} definition: {value:?}"
                                    )
                                }),
                        )
                    }
                    _ => None,
                }
            })
            .unwrap_or_else(|| Err(format!("Lensfun header is missing {name}")))
    }

    Ok(LensfunVersion::new(
        define(contents, "LF_VERSION_MAJOR")?,
        define(contents, "LF_VERSION_MINOR")?,
        define(contents, "LF_VERSION_MICRO")?,
    ))
}

pub fn validate_supported_lensfun_version(value: &str) -> Result<LensfunVersion, String> {
    let version = parse_lensfun_version(value)?;
    if !(MIN_SUPPORTED..=MAX_SUPPORTED).contains(&version) {
        return Err(format!(
            "unsupported Lensfun {version}; AuRaw supports Lensfun {MIN_SUPPORTED} through {MAX_SUPPORTED} because its generated FFI bindings and runtime ABI contract target the stable 0.3.x interface"
        ));
    }
    Ok(version)
}
