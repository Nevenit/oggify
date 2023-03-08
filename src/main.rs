extern crate env_logger;
extern crate librespot_audio;
extern crate librespot_core;
extern crate librespot_metadata;
#[macro_use]
extern crate log;
extern crate regex;
extern crate scoped_threadpool;
extern crate tokio_core;
extern crate sanitize_filename;

use std::env;
use std::fs::File;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use env_logger::{Builder, Env};
use librespot_audio::{AudioDecrypt, AudioFile};
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_core::spotify_id::{SpotifyId, SpotifyIdError};
use librespot_metadata::{Artist, FileFormat, Metadata, Track, Album};
use log::Level::Error;
use regex::Regex;
use scoped_threadpool::Pool;
use tokio_core::reactor::Core;
use slugify::slugify;
use id3::{Tag, TagLike, Version};

fn main() {
    Builder::from_env(Env::default().default_filter_or("info")).init();

    //Verify the number of arguments
    let args: Vec<_> = env::args().collect();
    assert!(args.len() == 4, format!("Usage: {} user password tracks_file", args[0]));

    //Create the sesion
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let session_config = SessionConfig::default();
    let credentials = Credentials::with_password(args[1].to_owned(), args[2].to_owned());
    info!("Connecting ...");

    // Connect to the session
    let session = core
        .run(Session::connect(session_config, credentials, None, handle))
        .unwrap();
    info!("Connected!");

    // Create the threadpool (want to get rid of it later)
    let mut threadpool = Pool::new(1);

    // Create vector storing all tracks
    let mut track_list: Vec<(Track, SpotifyId, Vec<String>)> = vec![];

    if let Ok(lines) = read_lines(args[3].to_owned()) {
        // Add all unique tracks to the list
        for link in lines {
            info!("--------------------------------------------------------------");
            let linkCopy = link.unwrap().clone();
            let songLink = linkCopy.as_str();
            let songId = extractSongId(songLink).expect("Failed to extract song id");
            info!("SongId: {}", songId.to_base62());

            let track = getTrack(songId, &mut core, &session);
            //println!("Track: {}", track.name);

            let mut artists_strs = getArtists(&track, &mut core, &session);

            //println!("Artist: {}", artists_strs[0]);

            // Store the artist in the track list and dont and to track list if file is already downloaded
            let fname = windows_compatible_file_name(format!("{} - {} [{}].mp3", artists_strs.join(", "), track.name, songId.to_base62()));

            if Path::new(&fname).exists() {
                info!("{} - is already downloaded", fname);
                continue;
            }
            info!("{} - Added to download list!", fname);
            track_list.push((track, songId, artists_strs));
        }
    }

    for item in track_list {
        let file_id = item.0.files.get(&FileFormat::OGG_VORBIS_320)
            .or(item.0.files.get(&FileFormat::OGG_VORBIS_160))
            .or(item.0.files.get(&FileFormat::OGG_VORBIS_96))
            .expect("Could not find a OGG_VORBIS format for the track.");

        let key = core.run(session.audio_key().request(item.0.id, *file_id)).expect("Cannot get audio key");
        let mut encrypted_file = core.run(AudioFile::open(&session, *file_id, 320, true)).unwrap();
        let mut buffer = Vec::new();
        let mut read_all: Result<usize> = Ok(0);
        let fetched = AtomicBool::new(false);
        threadpool.scoped(|scope|{
            scope.execute(||{
                read_all = encrypted_file.read_to_end(&mut buffer);
                fetched.store(true, Ordering::Release);
            });
            while !fetched.load(Ordering::Acquire) {
                core.turn(Some(Duration::from_millis(100)));
            }
        });
        read_all.expect("Cannot read file stream");
        let mut decrypted_buffer = Vec::new();
        AudioDecrypt::new(key, &buffer[..]).read_to_end(&mut decrypted_buffer).expect("Cannot decrypt stream");

        let mut fname = String::new();
        let dir = String::from("output/");
        fname.push_str(&dir);
        fname.push_str(&windows_compatible_file_name(format!("{} - {} [{}].mp3", item.2.join(", "), item.0.name, item.1.to_base62())));
        info!("Filename: {}", fname);
        std::fs::write(&fname, &decrypted_buffer[0xa7..]).expect("Cannot write decrypted track");

        set_metadata(fname, item.0, item.2);
    }
}

pub fn extractSongId(link: &str) -> std::result::Result<SpotifyId, SpotifyIdError> {
    let spotify_uri: Regex = Regex::new(r"spotify:track:([[:alnum:]]+)").unwrap();
    let spotify_url: Regex = Regex::new(r"open\.spotify\.com/track/([[:alnum:]]+)").unwrap();

    let uriCapture = spotify_uri.captures(link);
    let urlCapture = spotify_url.captures(link);

    if uriCapture.is_some() {
        return SpotifyId::from_base62(&uriCapture.unwrap()[1]);
    } else if urlCapture.is_some() {
        return SpotifyId::from_base62(&urlCapture.unwrap()[1]);
    }
    Err(SpotifyIdError)
}

pub fn getTrack(songId: SpotifyId, core: &mut Core, session: &Session) -> Track {
    info!("Getting track {}...", songId.to_base62());
    let mut track = core.run(Track::get(session, songId)).expect("Cannot get track metadata");
    if !track.available {
        let mut pot_track = None;
        warn!("Track {} is not available, finding alternative...", songId.to_base62());
        for alt_track in track.alternatives.iter() {
            let potential_track = core.run(Track::get(session, *alt_track)).expect("Cannot get track metadata");
            if potential_track.available {
                pot_track = Some(potential_track);
                warn!("Found track alternative {} -> {}", songId.to_base62(), track.id.to_base62());
                break;
            }
        }
        track = pot_track.expect(&format!("Could not find alternative for track {}", songId.to_base62()));
    }
    track
}

pub fn getArtists(track: &Track, core: &mut Core, session: &Session) -> Vec<String> {
    let mut artist_vec: Vec<String> = vec![];
    let local_track = track.clone();
    for artist in local_track.artists {
        let artist_id = core.run(Artist::get(session, artist)).expect("Cannot get artist metadata").name;
        artist_vec.push(artist_id);
    }
    artist_vec
}

fn windows_compatible_file_name(input: String) -> String {
    return sanitize_filename::sanitize(input);
}

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
    where P: AsRef<Path>, {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

fn set_metadata(file: String, track: Track, artists: Vec<String>) {
    if let Ok(mut tag) = Tag::read_from_path(&file) {
        //tag.set_album(track.album);
        tag.set_artist(artists.join(", "));
        tag.set_title(track.name);
        tag.set_text("Spotify id", track.id.to_base62());
        tag.write_to_path(&file, Version::Id3v24);
    }
}

/*
ThingsNotToDo:
-Add playlist support
-Dont download existing files - DONE
-Add metadata to song files
-Find a way to support duplicate file names without having the song id in the file name...
 */