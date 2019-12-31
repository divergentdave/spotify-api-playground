use log::error;
use rspotify::spotify::{
    client::Spotify,
    model::playlist::PlaylistTrack,
    oauth2::{SpotifyClientCredentials, SpotifyOAuth},
};
use serde::Deserialize;
use std::collections::VecDeque;
use std::convert::TryInto;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    IO(std::io::Error),
    Yaml(serde_yaml::Error),
    Sled(sled::Error),
    Failure(failure::Error),
    Cbor(serde_cbor::Error),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::IO(error)
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(error: serde_yaml::Error) -> Error {
        Error::Yaml(error)
    }
}

impl From<sled::Error> for Error {
    fn from(error: sled::Error) -> Error {
        Error::Sled(error)
    }
}

impl From<failure::Error> for Error {
    fn from(error: failure::Error) -> Error {
        Error::Failure(error)
    }
}

impl From<serde_cbor::Error> for Error {
    fn from(error: serde_cbor::Error) -> Error {
        Error::Cbor(error)
    }
}

const SCOPES: [&str; 4] = [
    "playlist-read-collaborative",
    "playlist-read-private",
    "user-library-read",
    "user-read-private",
];

#[derive(Deserialize)]
struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
    pub device_id: Option<String>,
}

impl ClientConfig {
    fn new() -> Self {
        Self {
            client_id: "".to_string(),
            client_secret: "".to_string(),
            device_id: None,
        }
    }

    fn load_config(&mut self) -> Result<(), Error> {
        let path = PathBuf::from("/home/david/.config/spotify-tui/client.yml");
        let data = std::fs::read_to_string(&path)?;
        let config_yml: ClientConfig = serde_yaml::from_str(&data)?;

        self.client_id = config_yml.client_id;
        self.client_secret = config_yml.client_secret;
        self.device_id = config_yml.device_id;

        Ok(())
    }
}

fn auth() -> Result<Spotify, Error> {
    let mut client_config = ClientConfig::new();
    client_config.load_config()?;

    let mut oauth = SpotifyOAuth::default()
        .client_id(&client_config.client_id)
        .client_secret(&client_config.client_secret)
        .redirect_uri("http://localhost:8888/callback")
        .cache_path(PathBuf::from(
            "/home/david/.config/spotify-tui/.spotify_token_cache.json",
        ))
        .scope(&SCOPES.join(" "))
        .build();
    let token = oauth
        .get_cached_token()
        .expect("Spotify authentication token not present");
    let client_creds = SpotifyClientCredentials::default()
        .token_info(token)
        .build();
    let spotify = Spotify::default()
        .client_credentials_manager(client_creds)
        .build();
    Ok(spotify)
}

const SEARCH_LIMIT: u32 = 20;

pub struct CachingSpotify {
    spotify: Spotify,
    db: sled::Db,
}

impl CachingSpotify {
    pub fn new() -> Result<CachingSpotify, Error> {
        Ok(CachingSpotify {
            spotify: auth()?,
            db: sled::open("cache")?,
        })
    }

    pub fn playlist_tracks(&self, playlist_id: &str, force: bool) -> Result<PlaylistTracks, Error> {
        let length_tree = self.db.open_tree("playlist_length")?;
        let tracks_tree = self.db.open_tree("playlist_tracks")?;
        let total = if force {
            None
        } else {
            match length_tree.get(playlist_id)? {
                Some(ivec) => match ivec.as_ref().try_into() {
                    Ok(array) => Some(u32::from_be_bytes(array)),
                    Err(_) => None,
                },
                None => None,
            }
        };
        let mut key = playlist_id.to_string().into_bytes();
        key.extend(&[0, 0, 0, 0]);
        match total {
            Some(total) => Ok(PlaylistTracks {
                spotify: &self.spotify,
                playlist_id: playlist_id.to_string(),
                total,
                offset: 0,
                key,
                buffer: VecDeque::new(),
                tree: tracks_tree,
            }),
            None => {
                let first_page = self.spotify.user_playlist_tracks(
                    "", // user id, no longer required
                    playlist_id,
                    None, // fields
                    Some(SEARCH_LIMIT),
                    Some(0), // playlist_offset
                    None,    // market
                )?;
                length_tree.insert(playlist_id, &first_page.total.to_be_bytes())?;
                Ok(PlaylistTracks {
                    spotify: &self.spotify,
                    playlist_id: playlist_id.to_string(),
                    total: first_page.total,
                    offset: 0,
                    key,
                    buffer: first_page.items.into(),
                    tree: tracks_tree,
                })
            }
        }
    }
}

pub struct PlaylistTracks<'a> {
    spotify: &'a Spotify,
    playlist_id: String,
    total: u32,
    offset: u32,
    key: Vec<u8>,
    buffer: VecDeque<PlaylistTrack>,
    tree: sled::Tree,
}

impl PlaylistTracks<'_> {
    fn update_key(&mut self, offset: u32) {
        let offset_bytes = u32::to_be_bytes(offset);
        let offset_position = self.key.len() - 4;
        (&mut self.key[offset_position..]).copy_from_slice(&offset_bytes[..]);
    }
}

impl Iterator for PlaylistTracks<'_> {
    type Item = Result<PlaylistTrack, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.total {
            return None;
        }
        self.update_key(self.offset);
        match self.tree.get(&self.key) {
            Ok(Some(ivec)) => match serde_cbor::from_reader(ivec.as_ref()) {
                Ok(track) => {
                    self.buffer.pop_front();
                    self.offset += 1;
                    return Some(Ok(track));
                }
                Err(e) => error!("Deserialization error reading from cache: {:?}", e),
            },
            Ok(None) => {}
            Err(e) => error!("Database error reading from cache: {:?}", e),
        }
        if let Some(track) = self.buffer.pop_front() {
            self.offset += 1;
            return Some(Ok(track));
        }
        let next_page = match self.spotify.user_playlist_tracks(
            "", // user id, no longer required
            &self.playlist_id,
            None, // fields
            Some(SEARCH_LIMIT),
            Some(self.offset), // playlist_offset
            None,              // market
        ) {
            Ok(next_page) => next_page,
            Err(e) => return Some(Err(Error::Failure(e))),
        };
        for (i, track) in next_page.items.iter().enumerate() {
            let mut serialized = Vec::new();
            if let Err(e) = serde_cbor::to_writer(&mut serialized, track) {
                return Some(Err(Error::Cbor(e)));
            }
            self.update_key(self.offset + i as u32);
            if let Err(e) = self.tree.insert(&self.key, serialized) {
                return Some(Err(Error::Sled(e)));
            }
        }
        self.buffer = next_page.items.into();
        let maybe_track = self.buffer.pop_front();
        self.offset += 1;
        maybe_track.map(Result::Ok)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
