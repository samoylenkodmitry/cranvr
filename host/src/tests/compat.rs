use super::status_line;

// The texts of ALVR v20.14.1's warnings about a client of another version. Discovery logs two,
// one per discovery method. The handshake of a trusted client logs one more.
#[test]
fn every_version_warning_of_alvr_gives_the_notice() {
    for warning in [
        "Found incompatible client! Upgrade or downgrade\nExpected protocol ID 1, Found 2",
        "Found incompatible client quest.client! Please use stable server or nightly client.\nProtocols: server=20, client=21-dev",
        "Trusted client is incompatible! Expected protocol ID: 1, found: 2",
        "Found old client. Please upgrade",
    ] {
        let line = status_line(warning).expect(warning);
        assert_eq!(line["problem"], "incompatible_client");
        assert_eq!(line["detail"], warning);
    }
    assert!(status_line("Found unrelated packet during discovery").is_none());
}
