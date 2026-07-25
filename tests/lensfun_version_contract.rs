#[path = "../build_support/lensfun_version.rs"]
mod lensfun_version;

use lensfun_version::{
    parse_lensfun_header_version, parse_lensfun_version, validate_supported_lensfun_version,
    LensfunVersion,
};

#[test]
fn supported_lensfun_versions_are_accepted() {
    for version in ["0.3.2", "0.3.3", "0.3.4", "0.3.4.0", "0.3.4-2"] {
        assert!(
            validate_supported_lensfun_version(version).is_ok(),
            "{version} should be supported"
        );
    }
}

#[test]
fn unsupported_lensfun_versions_are_rejected_clearly() {
    for version in ["0.3.1", "0.3.95", "0.4.0", "1.0.0"] {
        let error = validate_supported_lensfun_version(version)
            .expect_err("unsupported version should fail");
        assert!(error.contains("unsupported Lensfun"));
        assert!(error.contains("0.3.2 through 0.3.4"));
    }
}

#[test]
fn malformed_lensfun_versions_are_rejected() {
    assert!(parse_lensfun_version("0.3").is_err());
    assert!(parse_lensfun_version("zero.3.4").is_err());
    assert!(parse_lensfun_version("0.3.4.1").is_err());
    assert_eq!(
        parse_lensfun_version("0.3.4+packager.1").unwrap(),
        LensfunVersion::new(0, 3, 4)
    );
}

#[test]
fn selected_header_version_must_be_parsed_and_supported() {
    let header = r#"
#define LF_VERSION_MAJOR 0
#define LF_VERSION_MINOR 3
#define LF_VERSION_MICRO 4
"#;
    let version = parse_lensfun_header_version(header).unwrap();
    assert_eq!(version, LensfunVersion::new(0, 3, 4));
    assert!(validate_supported_lensfun_version(&version.to_string()).is_ok());
}

#[test]
fn incomplete_or_invalid_header_versions_are_rejected() {
    assert!(parse_lensfun_header_version("#define LF_VERSION_MAJOR 0").is_err());
    assert!(parse_lensfun_header_version(
        "#define LF_VERSION_MAJOR zero\n#define LF_VERSION_MINOR 3\n#define LF_VERSION_MICRO 4"
    )
    .is_err());
}
