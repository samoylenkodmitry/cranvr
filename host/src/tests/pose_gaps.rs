use super::*;
use std::time::Duration;

#[test]
fn gaps_report_rate_and_spread() {
    let start = Instant::now();
    // nine gaps of 3 ms and one of 20 ms over one second
    for i in 0..10u64 {
        record_at(start + Duration::from_millis(3 * i));
    }
    record_at(start + Duration::from_millis(47));
    let report = take(1.0);
    assert_eq!(report["hz"], 10.0);
    assert!((report["p50"].as_f64().unwrap() - 3.0).abs() < 0.01);
    assert!((report["max"].as_f64().unwrap() - 20.0).abs() < 0.01);
    // the report empties the list
    assert_eq!(take(1.0), Value::Null);
}
