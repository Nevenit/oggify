extern crate env_logger;
extern crate librespot_audio;
extern crate librespot_core;
extern crate librespot_metadata;
#[macro_use]
extern crate log;
extern crate regex;
extern crate scoped_threadpool;
extern crate tokio_core;

use std::env;
use std::io::{self, BufRead, Read, Result};
use std::io::Write;
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

fn main() {
    Builder::from_env(Env::default().default_filter_or("info")).init();

    let args: Vec<_> = env::args().collect();
    assert!(args.len() == 3 || args.len() == 4, "Usage: {} user password [helper_script] < tracks_file", args[0]);

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let session_config = SessionConfig::default();
    let credentials = Credentials::with_password(args[1].to_owned(), args[2].to_owned());
    info!("Connecting ...");
    let session = core
        .run(Session::connect(session_config, credentials, None, handle))
        .unwrap();
    info!("Connected!");

    let mut threadpool = Pool::new(1);

    let spotify_uri = Regex::new(r"spotify:track:([[:alnum:]]+)").unwrap();
    let spotify_url = Regex::new(r"open\.spotify\.com/track/([[:alnum:]]+)").unwrap();

    for link in io::stdin().lock().lines() {
        let linkCopy = link.unwrap().clone();
        let songLink = linkCopy.as_str();
        let songId = extractSongId(songLink).expect("Failed to extract song id");
        println!("SongId: {}", songId.to_base62());

        let track = getTrack(songId, &mut core, &session);
        println!("Track: {}", track.name);

        let mut artists_strs = getArtists(&track, &mut core, &session);

        println!("Artist: {}", artists_strs[0]);

        //debug!("File formats: {}", track.files.keys().map(|filetype|format!("{:?}", filetype)).collect::<Vec<_>>().join(" "));

        let file_id = track.files.get(&FileFormat::OGG_VORBIS_320)
            .or(track.files.get(&FileFormat::OGG_VORBIS_160))
            .or(track.files.get(&FileFormat::OGG_VORBIS_96))
            .expect("Could not find a OGG_VORBIS format for the track.");





        let key = core.run(session.audio_key().request(track.id, *file_id)).expect("Cannot get audio key");
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
        if args.len() == 3 {
            let fname = format!("{} - {}.ogg", artists_strs.join(", "), track.name);
            std::fs::write(&fname, &decrypted_buffer[0xa7..]).expect("Cannot write decrypted track");
            info!("Filename: {}", fname);
        } else {
            let album = core.run(Album::get(&session, track.album)).expect("Cannot get album metadata");
            let mut cmd = Command::new(args[3].to_owned());
            cmd.stdin(Stdio::piped());
            cmd.arg(songId.to_base62()).arg(track.name).arg(album.name).args(artists_strs.iter());
            let mut child = cmd.spawn().expect("Could not run helper program");
            let pipe = child.stdin.as_mut().expect("Could not open helper stdin");
            pipe.write_all(&decrypted_buffer[0xa7..]).expect("Failed to write to stdin");
            assert!(child.wait().expect("Out of ideas for error messages").success(), "Helper script returned an error");
        }
    }



    /*
    io::stdin().lock().lines()
        .filter_map(|line|
            line.ok().and_then(|str|
                spotify_uri.captures(&str).or(spotify_url.captures(&str))
                    .or_else(|| { warn!("Cannot parse track from string {}", str); None })
                    .and_then(|capture|SpotifyId::from_base62(&capture[1]).ok())))
        .for_each(|id|{
            info!("Getting track {}...", id.to_base62());
            let mut track = core.run(Track::get(&session, id)).expect("Cannot get track metadata");
            if !track.available {
                warn!("Track {} is not available, finding alternative...", id.to_base62());
                let alt_track = track.alternatives.iter().find_map(|id|{
                    let alt_track = core.run(Track::get(&session, *id)).expect("Cannot get track metadata");
                    match alt_track.available {
                        true => Some(alt_track),
                        false => None
                    }
                });
                track = alt_track.expect(&format!("Could not find alternative for track {}", id.to_base62()));
                warn!("Found track alternative {} -> {}", id.to_base62(), track.id.to_base62());
            }
            let artists_strs: Vec<_> = track.artists.iter().map(|id|core.run(Artist::get(&session, *id)).expect("Cannot get artist metadata").name).collect();
            debug!("File formats: {}", track.files.keys().map(|filetype|format!("{:?}", filetype)).collect::<Vec<_>>().join(" "));
            let file_id = track.files.get(&FileFormat::OGG_VORBIS_320)
                .or(track.files.get(&FileFormat::OGG_VORBIS_160))
                .or(track.files.get(&FileFormat::OGG_VORBIS_96))
                .expect("Could not find a OGG_VORBIS format for the track.");
            let key = core.run(session.audio_key().request(track.id, *file_id)).expect("Cannot get audio key");
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
            if args.len() == 3 {
                let fname = format!("{} - {}.ogg", artists_strs.join(", "), track.name);
                std::fs::write(&fname, &decrypted_buffer[0xa7..]).expect("Cannot write decrypted track");
                info!("Filename: {}", fname);
            } else {
                let album = core.run(Album::get(&session, track.album)).expect("Cannot get album metadata");
                let mut cmd = Command::new(args[3].to_owned());
                cmd.stdin(Stdio::piped());
                cmd.arg(id.to_base62()).arg(track.name).arg(album.name).args(artists_strs.iter());
                let mut child = cmd.spawn().expect("Could not run helper program");
                let pipe = child.stdin.as_mut().expect("Could not open helper stdin");
                pipe.write_all(&decrypted_buffer[0xa7..]).expect("Failed to write to stdin");
                assert!(child.wait().expect("Out of ideas for error messages").success(), "Helper script returned an error");
            }
        });

     */
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