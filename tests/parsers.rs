// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parser tests pinned to the payload shapes Spotify and Soloist actually send.

use serde_json::json;
use spotlite::model::*;

#[test]
fn track_parses_core_fields() {
    let v = json!({
        "uri": "spotify:track:abc",
        "name": "Nightcall",
        "artists": [{"name": "Kavinsky"}, {"name": "Phoebe"}],
        "album": {"name": "OutRun", "images": [
            {"url": "https://i.scdn.co/large", "width": 640},
            {"url": "https://i.scdn.co/small", "width": 64}
        ]},
        "duration_ms": 253000,
        "is_playable": true
    });
    let t = track_from(&v).unwrap();
    assert_eq!(t.uri, "spotify:track:abc");
    assert_eq!(t.title(), "Nightcall");
    assert_eq!(t.artists, "Kavinsky, Phoebe");
    assert_eq!(t.album, "OutRun");
    assert_eq!(t.duration_ms, 253000);
    assert_eq!(t.image.as_deref(), Some("https://i.scdn.co/small"));
}

#[test]
fn track_wrapper_accepts_both_item_shapes() {
    let modern = json!({"added_at": "x", "item": {"uri": "spotify:track:1", "name": "A"}});
    let legacy = json!({"added_at": "x", "track": {"uri": "spotify:track:2", "name": "B"}});
    assert_eq!(track_from_wrapped(&modern).unwrap().name, "A");
    assert_eq!(track_from_wrapped(&legacy).unwrap().name, "B");
}

#[test]
fn track_without_uri_is_skipped() {
    assert!(track_from(&json!({"name": "local file"})).is_none());
}

#[test]
fn playlist_reads_items_total_and_falls_back_to_tracks_total() {
    let new = json!({"id": "p1", "name": "Mine", "items": {"total": 12}});
    let old = json!({"id": "p2", "name": "Theirs", "tracks": {"total": 7}});
    assert_eq!(playlist_from(&new).unwrap().count, 12);
    assert_eq!(playlist_from(&old).unwrap().count, 7);
    assert_eq!(playlist_from(&new).unwrap().uri, "spotify:playlist:p1");
}

#[test]
fn album_year_is_truncated_to_four_digits() {
    let v = json!({"id": "a1", "name": "LP", "release_date": "1997-05-01", "total_tracks": 9});
    let a = album_from(&v).unwrap();
    assert_eq!(a.year, "1997");
    assert_eq!(a.count, 9);
}

#[test]
fn soloist_entity_becomes_a_track() {
    let v = json!({
        "uri": "spotify:track:xyz",
        "entity_type": "track",
        "decorations": {
            "identity": {"name": "Bloom"},
            "visual_identity": {"cover": [
                {"url": "https://i.scdn.co/small", "size": "small"},
                {"url": "https://i.scdn.co/large", "size": "large"}
            ]},
            "parent": {"entity": {"uri": "spotify:album:1", "decorations": {"identity": {"name": "Album X"}}}},
            "creators": [
                {"entity": {"decorations": {"identity": {"name": "ODESZA"}}}},
                {"entity": {"decorations": {"identity": {"name": "Guest"}}}}
            ],
            "playback": {"duration_ms": 210000}
        }
    });
    let t = track_from_entity(&v).unwrap();
    assert_eq!(t.name, "Bloom");
    assert_eq!(t.artists, "ODESZA, Guest");
    assert_eq!(t.album, "Album X");
    assert_eq!(t.duration_ms, 210000);
    assert_eq!(t.image.as_deref(), Some("https://i.scdn.co/large"));
}

#[test]
fn position_interpolates_and_clamps() {
    let mut p = Playback {
        playing: true,
        track: Some(Track {
            duration_ms: 60_000,
            ..Default::default()
        }),
        position_ms: 10_000,
        anchor_ms: now_ms().saturating_sub(5_000),
        ..Default::default()
    };
    let pos = p.position_now();
    assert!((14_000..16_000).contains(&pos), "got {pos}");
    p.playing = false;
    assert_eq!(p.position_now(), 10_000);
    p.playing = true;
    p.position_ms = 59_000;
    p.anchor_ms = now_ms().saturating_sub(10_000);
    assert_eq!(p.position_now(), 60_000);
}

#[test]
fn time_formatting() {
    assert_eq!(fmt_time(0), "0:00");
    assert_eq!(fmt_time(61_000), "1:01");
    assert_eq!(fmt_time(3_600_000), "60:00");
}

#[test]
fn query_escaping_covers_reserved_characters() {
    assert_eq!(esc("a b&c?d#e+f%g"), "a%20b%26c%3Fd%23e%2Bf%25g");
}
