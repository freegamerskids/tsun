//! The TIDAL worker thread: it owns `Client` for the whole run, so every blocking call (device
//! auth polling, search, playback control) happens here and never on the frame.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use rune::Waker;

use crate::app::{Cmd, Ev};
use crate::audio::{Outcome, OutputOptions, Player, Source, Volume};
use crate::tidal::Client;

/// Owns the backend for the whole run. Every blocking call happens here.
pub(crate) fn worker(cmd_rx: Receiver<Cmd>, ev_tx: Sender<Ev>, waker: Waker) {
    let emit = |ev: Ev| {
        let _ = ev_tx.send(ev);
        waker.as_ref()();
    };

    let mut client = match Client::new() {
        Ok(client) => {
            let logged_in = client.is_logged_in();
            emit(Ev::Ready { logged_in });
            Some(client)
        }
        Err(error) => {
            emit(Ev::InitFailed(error));
            None
        }
    };

    let mut playing_seq = None;
    let mut last_position = None;
    // Local playback shares the audio layer with the TIDAL client but has its own player,
    // output device and normalization state. The volume handle is one shared control.
    let local_volume = Volume::new();
    let mut local: Option<Player> = None;
    let mut output_device: Option<String> = None;
    let mut normalize = false;
    loop {
        let cmd = match cmd_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if let (Some(seq), Some(player)) = (playing_seq, local.as_mut()) {
                    if let Some(id) = player.take_gapless_switch() {
                        last_position = None;
                        let output = player.output_info().map(|info| info.label());
                        emit(Ev::GaplessAdvanced { seq, id, output });
                        continue;
                    }
                    if let Some(result) = player.poll() {
                        playing_seq = None;
                        match result {
                            Ok(Outcome::Finished) => emit(Ev::TrackFinished { seq }),
                            Ok(Outcome::Stopped) => emit(Ev::Stopped),
                            Err(error) => emit(Ev::PlaybackEnded { seq, error: Some(error) }),
                        }
                        local = None;
                    } else {
                        let elapsed = player.position_seconds();
                        if last_position != Some(elapsed) {
                            last_position = Some(elapsed);
                            emit(Ev::PlaybackProgress { seq, elapsed });
                        }
                    }
                    continue;
                }
                if let (Some(seq), Some(client)) = (playing_seq, client.as_mut()) {
                    // A gapless hand-off means the current player is still playing: report the
                    // new track without tearing anything down.
                    if let Some(id) = client.take_gapless_switch() {
                        last_position = None;
                        let output = client.output_info().map(|info| info.label());
                        emit(Ev::GaplessAdvanced { seq, id, output });
                        continue;
                    }
                    if let Some(result) = client.poll_playback() {
                        playing_seq = None;
                        match result {
                            Ok(Outcome::Finished) => emit(Ev::TrackFinished { seq }),
                            Ok(Outcome::Stopped) => emit(Ev::Stopped),
                            Err(error) => emit(Ev::PlaybackEnded { seq, error: Some(error) }),
                        }
                    }
                    else if let Some(elapsed) = client.playback_position() {
                        if last_position != Some(elapsed) {
                            last_position = Some(elapsed);
                            emit(Ev::PlaybackProgress { seq, elapsed });
                        }
                    }
                }
                continue;
            }
        };
        match cmd {
            Cmd::Init => match Client::new() {
                Ok(new_client) => {
                    let logged_in = new_client.is_logged_in();
                    client = Some(new_client);
                    emit(Ev::Ready { logged_in });
                }
                Err(error) => emit(Ev::InitFailed(error)),
            },
            Cmd::Login => {
                let Some(client) = client.as_mut() else {
                    emit(Ev::LoginFailed("The TIDAL client is not ready.".to_owned()));
                    continue;
                };
                match client.login() {
                    Ok(url) => {
                        emit(Ev::LoginUrl(url));
                        match client.complete_login() {
                            Ok(()) => emit(Ev::LoginDone),
                            Err(error) => emit(Ev::LoginFailed(error)),
                        }
                    }
                    Err(error) => emit(Ev::LoginFailed(error)),
                }
            }
            Cmd::StartPkce => {
                let Some(client) = client.as_mut() else {
                    emit(Ev::PkceFailed("The TIDAL client is not ready.".to_owned()));
                    continue;
                };
                match client.start_pkce() {
                    Ok(url) => emit(Ev::PkceUrl(url)),
                    Err(error) => emit(Ev::PkceFailed(error)),
                }
            }
            Cmd::FinishPkce { url } => {
                let Some(client) = client.as_mut() else {
                    emit(Ev::PkceFailed("The TIDAL client is not ready.".to_owned()));
                    continue;
                };
                match client.finish_pkce(&url) {
                    Ok(()) => emit(Ev::LoginDone),
                    Err(error) => emit(Ev::PkceFailed(error)),
                }
            }
            Cmd::SearchAll { seq, query } => {
                let Some(client) = client.as_mut() else {
                    emit(Ev::SearchFailed { seq, error: "The TIDAL client is not ready.".to_owned() });
                    continue;
                };
                let result = client.search_all(&query);
                emit(Ev::SearchAllDone { seq, result });
            }
            Cmd::Suggestions { seq, query } => {
                let result = match client.as_mut() {
                    Some(client) => client.search_suggestions(&query),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Suggestions { seq, result });
            }
            Cmd::Explore => {
                let result = match client.as_mut() {
                    Some(client) => client.explore(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::ExploreResult(result));
            }
            Cmd::NewReleases => {
                let result = match client.as_mut() {
                    Some(client) => client.new_releases(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::NewReleases(result));
            }
            Cmd::HomeSections => {
                let result = match client.as_mut() {
                    Some(client) => client.home_sections(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::HomeSections(result));
            }
            Cmd::Lyrics { track_id } => {
                let result = match client.as_mut() {
                    Some(client) => client.lyrics(track_id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Lyrics { track_id, result });
            }
            Cmd::Profile => {
                let result = match client.as_mut() {
                    Some(client) => client.profile(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Profile(result));
            }
            Cmd::Home => {
                let result = match client.as_mut() {
                    Some(client) => client.home_tracks(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::HomeResult(result));
                let recommendations = match client.as_mut() {
                    Some(client) => client.home_recommendations(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::HomeRecommendations(recommendations));
            }
            Cmd::Playlists => {
                let result = match client.as_mut() {
                    Some(client) => client.playlists(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Playlists(result));
            }
            Cmd::Mixes => {
                let result = match client.as_mut() {
                    Some(client) => client.mixes(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Mixes(result));
            }
            Cmd::Library => {
                let result = match client.as_mut() {
                    Some(client) => client.library(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Library(result));
            }
            Cmd::FavoriteIds => {
                let result = match client.as_mut() {
                    Some(client) => client.favorite_ids(),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Favorites(result));
            }
            Cmd::SetFavorite { kind, id, on } => {
                let error = match client.as_mut() {
                    Some(client) => client.set_favorite(kind, &id, on).err(),
                    None => Some("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::FavoriteChanged { kind, id, on, error });
            }
            Cmd::OpenAlbum(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.album(id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Album { id, result });
            }
            Cmd::OpenArtist(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.artist_page(id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Artist { id, result });
            }
            Cmd::ArtistRadio(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.artist_radio(id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::Radio { id, result });
            }
            Cmd::OpenMix(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.mix_tracks(&id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::MixTracks { id, result });
            }
            Cmd::OpenPlaylist(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.playlist_tracks(&id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistTracks { id, result });
            }
            Cmd::PlaylistMeta(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.playlist(&id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistMeta { id, result });
            }
            Cmd::CreatePlaylist { title, description, tracks } => {
                let Some(client) = client.as_mut() else {
                    emit(Ev::PlaylistCreated {
                        result: Err("The TIDAL client is not ready.".to_owned()),
                        added: None,
                    });
                    continue;
                };
                match client.create_playlist(&title, &description) {
                    Ok(playlist) => {
                        // A create that came from a menu's "New playlist…" also fills the new
                        // playlist, so the user gets what they asked for in one step.
                        let added = (!tracks.is_empty()).then(|| {
                            client
                                .add_to_playlist(&playlist.id, &tracks)
                                .map(|()| tracks.len())
                        });
                        emit(Ev::PlaylistCreated { result: Ok(playlist), added });
                    }
                    Err(error) => emit(Ev::PlaylistCreated { result: Err(error), added: None }),
                }
            }
            Cmd::AddToPlaylist { id, track_ids } => {
                let count = track_ids.len();
                let result = match client.as_mut() {
                    Some(client) => client.add_to_playlist(&id, &track_ids),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistAdded { id, count, result });
            }
            Cmd::RemoveFromPlaylist { id, index } => {
                let result = match client.as_mut() {
                    Some(client) => client.remove_from_playlist(&id, index),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistRemoved { id, index, result });
            }
            Cmd::MovePlaylistItem { id, from, to } => {
                let result = match client.as_mut() {
                    Some(client) => client.move_playlist_item(&id, from, to),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistMoved { id, from, to, result });
            }
            Cmd::DeletePlaylist(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.delete_playlist(&id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistDeleted { id, result });
            }
            Cmd::RenamePlaylist { id, title, description } => {
                let result = match client.as_mut() {
                    Some(client) => client.rename_playlist(&id, &title, &description),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::PlaylistRenamed { id, title, description, result });
            }
            Cmd::TrackRadio(id) => {
                let result = match client.as_mut() {
                    Some(client) => client.track_radio(id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::TrackRadio(result));
            }
            Cmd::TrackCredits(track_id) => {
                let result = match client.as_mut() {
                    Some(client) => client.track_credits(track_id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::TrackCredits { track_id, result });
            }
            Cmd::AlbumExtras(id) => {
                let result = match client.as_mut() {
                    Some(client) => Ok(client.album_extras(id)),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::AlbumExtras { id, result });
            }
            Cmd::ArtistTopTracks { id, offset } => {
                let result = match client.as_mut() {
                    Some(client) => {
                        client.artist_top_tracks_all(id, offset, crate::tidal::MAX_TOP_TRACKS)
                    }
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::ArtistTopTracks { id, result });
            }
            Cmd::OpenVideo { seq, id } => {
                let volume = client
                    .as_ref()
                    .map(|client| client.volume())
                    .unwrap_or_else(crate::audio::Volume::new);
                let result = match client.as_mut() {
                    Some(client) => client.video_stream(id),
                    None => Err("The TIDAL client is not ready.".to_owned()),
                };
                emit(Ev::VideoReady { seq, id, result, volume });
            }
            Cmd::PlayLocal { seq, id, path, quality, replaygain } => {
                // A local play supersedes anything either player was doing.
                local = None;
                if let Some(client) = client.as_mut() {
                    client.stop();
                }
                playing_seq = None;
                last_position = None;
                let gain = if normalize { replaygain.unwrap_or(1.0) } else { 1.0 };
                local_volume.set_normalization_gain(gain);
                match Player::start(
                    Source::File(path),
                    local_volume.clone(),
                    OutputOptions { device: output_device.clone() },
                ) {
                    Ok(player) => {
                        playing_seq = Some(seq);
                        last_position = None;
                        let output = player.output_info().map(|info| info.label());
                        emit(Ev::Playing { seq, id, quality, output });
                        local = Some(player);
                    }
                    Err(error) => emit(Ev::PlayFailed { seq, error }),
                }
            }
            Cmd::Play { seq, id } => {
                // A local id is never a TIDAL track id; it must never reach the TIDAL play
                // endpoint (the UI routes local files through `PlayLocal` instead).
                if crate::local::is_local_id(id) {
                    emit(Ev::PlayFailed {
                        seq,
                        error: "This is a local file, not a TIDAL track.".to_owned(),
                    });
                    continue;
                }
                local = None;
                let Some(client) = client.as_mut() else {
                    emit(Ev::PlayFailed { seq, error: "The TIDAL client is not ready.".to_owned() });
                    continue;
                };
                playing_seq = None;
                last_position = None;
                match client.play(id) {
                    Ok(quality) => {
                        playing_seq = Some(seq);
                        last_position = None;
                        // The real format the device accepted, for the quality pill's tooltip.
                        let output = client.output_info().map(|info| info.label());
                        emit(Ev::Playing { seq, id, quality, output });
                    }
                    Err(error) => emit(Ev::PlayFailed { seq, error }),
                }
            }
            Cmd::SetPaused { seq, paused } => {
                let result = if playing_seq == Some(seq) {
                    if let Some(player) = local.as_ref() {
                        player.set_paused(paused);
                        Ok(())
                    } else {
                        client.as_ref().ok_or_else(|| "The TIDAL client is not ready.".to_owned())
                            .and_then(|client| client.set_paused(paused))
                    }
                } else {
                    Err("The track is no longer playing.".to_owned())
                };
                emit(Ev::PauseChanged { seq, paused, error: result.err() });
            }
            Cmd::Seek { seq, seconds } => {
                if playing_seq == Some(seq) {
                    if let Some(player) = local.as_ref() {
                        player.seek(seconds);
                    } else if let Some(client) = client.as_ref() {
                        client.seek(seconds);
                    }
                }
            }
            Cmd::SetVolume(volume) => {
                local_volume.set_volume(volume);
                if let Some(client) = client.as_ref() {
                    client.volume().set_volume(volume);
                }
            }
            Cmd::SetMuted(muted) => {
                local_volume.set_muted(muted);
                if let Some(client) = client.as_ref() {
                    client.volume().set_muted(muted);
                }
            }
            Cmd::SetQuality(quality) => {
                if let Some(client) = client.as_mut() {
                    client.set_quality_preference(quality);
                }
            }
            Cmd::SetNormalize(flag) => {
                normalize = flag;
                if let Some(client) = client.as_mut() {
                    client.set_normalize(flag);
                }
            }
            Cmd::SetOutputDevice(device) => {
                output_device = device.clone();
                if let Some(player) = local.as_ref() {
                    player.set_output_device(device.clone());
                }
                if let Some(client) = client.as_mut() {
                    client.set_output_device(device);
                }
            }
            Cmd::Preload { id } => {
                // Best-effort: a preload that fails just means the next track starts the
                // ordinary way when its turn comes. Local ids go through `PreloadLocal`.
                if !crate::local::is_local_id(id) {
                    if let Some(client) = client.as_mut() {
                        let _ = client.preload(id);
                    }
                }
            }
            Cmd::PreloadLocal { id, path, replaygain } => {
                if let Some(player) = local.as_ref() {
                    let gain = if normalize { replaygain.unwrap_or(1.0) } else { 1.0 };
                    let _ = player.preload(id, Source::File(path), gain);
                }
            }
            Cmd::CancelPreload => {
                if let Some(player) = local.as_ref() {
                    player.cancel_preload();
                }
                if let Some(client) = client.as_mut() {
                    client.cancel_preload();
                }
            }
            Cmd::Stop => {
                playing_seq = None;
                local = None;
                if let Some(client) = client.as_mut() {
                    client.stop();
                }
                emit(Ev::Stopped);
            }
            Cmd::Logout => {
                playing_seq = None;
                local = None;
                if let Some(client) = client.as_mut() {
                    client.logout();
                }
                emit(Ev::Ready { logged_in: false });
            }
        }
    }
}
