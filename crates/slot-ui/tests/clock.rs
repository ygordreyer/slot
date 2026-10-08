use slot_ui::{clock_label, Polaroids};

#[test]
fn recent_states_read_relative_and_old_ones_read_absolute() {
    let now = "2026-08-09_14-36-05";
    assert_eq!(
        Polaroids::relative_time("2026-08-09_14-32-05", now),
        "4 min ago"
    );
    assert_eq!(
        Polaroids::relative_time("2026-08-09_03-00-00", now),
        "11 hr ago"
    );
    assert_eq!(
        Polaroids::relative_time("2026-08-08_20-00-00", now),
        "2026-08-08 20:00"
    );
    assert_eq!(
        Polaroids::relative_time("2025-01-02_09-05-00", now),
        "2025-01-02 09:05"
    );
}

#[test]
fn the_clock_is_24_hour_and_shows_no_seconds() {
    assert_eq!(
        clock_label("2026-08-09_21-07-00"),
        "21:07",
        "not 24 hour, or showing seconds"
    );
}

#[test]
fn a_twelve_hour_clock_reads_the_way_people_write_it() {
    use slot_ui::{date_time_text_as, hhmm_as};
    let at = |h: i64, m: i64| h * 3600 + m * 60;
    assert_eq!(hhmm_as(at(15, 7), true), "3:07 PM");
    assert_eq!(hhmm_as(at(0, 5), true), "12:05 AM");
    assert_eq!(hhmm_as(at(12, 0), true), "12:00 PM");
    assert_eq!(hhmm_as(at(9, 30), true), "9:30 AM");
    assert_eq!(hhmm_as(at(15, 7), false), "15:07");
    assert!(date_time_text_as(at(15, 7), true).ends_with(" 3:07 PM"));
}

#[test]
fn an_old_polaroid_is_dated_on_the_twelve_hour_clock_when_asked() {
    let now = "2026-08-09_14-36-05";
    assert_eq!(
        Polaroids::relative_time_as("2026-08-08_20-00-00", now, true),
        "2026-08-08 8:00 PM"
    );
    assert_eq!(
        Polaroids::relative_time_as("2026-08-09_14-32-05", now, true),
        "4 min ago",
        "a recent one reads the same on either clock"
    );
}
