use once_cell::sync::OnceCell;
use regex::Regex;
use rspotify::spotify::model::{album::SimplifiedAlbum, playlist::PlaylistTrack};
use spotify_api_playground::{CachingSpotify, Error, PlaylistTracks};
use std::cmp::Ordering;

fn parse_playlist_link(url: &str) -> Option<String> {
    static REGEX: OnceCell<Regex> = OnceCell::new();
    let regex = REGEX.get_or_init(|| {
        Regex::new(
            "^(?:https://open.spotify.com/playlist/|spotify:playlist:)?([0-9A-Za-z]{22})(:?\\?si=[-_0-9A-Za-z]*)?$",
        )
        .unwrap()
    });
    let captures = regex.captures(url)?;
    captures
        .get(1)
        .map(|cap_match| cap_match.as_str().to_string())
}

fn year(album: &SimplifiedAlbum) -> Option<u16> {
    let text = match &album.release_date {
        Some(text) => text,
        None => return None,
    };
    match album.release_date_precision.as_ref().map(String::as_str) {
        None => None,
        Some("day") => Some((&text[..4]).parse().unwrap()),
        Some("year") => Some(text.parse().unwrap()),
        Some(other) => panic!("unhandled precision: {}", other),
    }
}

fn playlist_track_sort_cmp(a: &PlaylistTrack, b: &PlaylistTrack) -> Ordering {
    match a.track.album.release_date.cmp(&b.track.album.release_date) {
        Ordering::Equal => {}
        other => return other,
    }
    for (a_artist, b_artist) in a.track.artists.iter().zip(b.track.artists.iter()) {
        match a_artist.name.cmp(&b_artist.name) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    match a.track.artists.len().cmp(&b.track.artists.len()) {
        Ordering::Equal => {}
        other => return other,
    }
    match a.track.album.name.cmp(&b.track.album.name) {
        Ordering::Equal => {}
        other => return other,
    }
    match a.track.track_number.cmp(&b.track.track_number) {
        Ordering::Equal => {}
        other => return other,
    }
    a.track.name.cmp(&b.track.name)
}

fn print_playlist(iter: PlaylistTracks) -> Result<(), Error> {
    let tracks: Result<Vec<PlaylistTrack>, Error> = iter.collect();
    let mut tracks = tracks?;
    println!("{} tracks", tracks.len());
    tracks.sort_unstable_by(playlist_track_sort_cmp);
    let no_url_string = "(no URL)".to_string();
    for playlist_track in tracks.into_iter() {
        let track = playlist_track.track;
        println!(
            "{}\t{} - {} ({}) {}",
            track.external_urls.get("spotify").unwrap_or(&no_url_string),
            track.name,
            track
                .artists
                .iter()
                .map(|artist| artist.name.as_ref())
                .collect::<Vec<&str>>()
                .join(", "),
            track.album.name,
            match year(&track.album) {
                Some(year) => year.to_string(),
                None => "(unknown year)".to_string(),
            },
        );
    }
    Ok(())
}

fn main() -> Result<(), Error> {
    simple_logger::init_with_level(log::Level::Warn).unwrap();
    let arg = match std::env::args().skip(1).next() {
        Some(arg) => arg,
        None => {
            println!(
                "This command expects a Spotify playlist link or ID as a command line argument"
            );
            return Ok(());
        }
    };
    let playlist_id = match parse_playlist_link(arg.as_str()) {
        Some(playlist_id) => playlist_id,
        None => {
            println!("Couldn't parse playlist ID from argument");
            return Ok(());
        }
    };
    let spotify = CachingSpotify::new()?;
    print_playlist(spotify.playlist_tracks(&playlist_id, false)?)?;
    Ok(())
}
