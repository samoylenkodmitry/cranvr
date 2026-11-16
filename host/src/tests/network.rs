use super::StreamWatch;

// One connection without poses happens when a headset leaves; two in a row point at the network.
#[test]
fn two_connections_without_poses_report_once() {
    let mut watch = StreamWatch::default();
    watch.connected();
    assert!(watch.disconnected(false).is_none());
    watch.connected();
    let report = watch.disconnected(false).expect("a report");
    assert_eq!(report["protocol"], "udp");
    watch.connected();
    assert!(watch.disconnected(false).is_none(), "the report comes once");
}

#[test]
fn a_connection_with_poses_starts_the_count_again() {
    let mut watch = StreamWatch::default();
    watch.connected();
    watch.disconnected(true);
    watch.connected();
    watch.pose();
    assert!(watch.disconnected(true).is_none());
    watch.connected();
    assert!(watch.disconnected(true).is_none());
    watch.connected();
    assert_eq!(watch.disconnected(true).expect("a report")["protocol"], "tcp");
}
