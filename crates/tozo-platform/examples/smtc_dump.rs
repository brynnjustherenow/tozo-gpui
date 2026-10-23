use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

fn main() {
    let command = std::env::args().nth(1);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let manager = Manager::RequestAsync()
        .expect("RequestAsync")
        .join()
        .expect("join manager");

    if let Some(command) = command {
        let sessions = manager.GetSessions().expect("GetSessions");
        let session = sessions
            .into_iter()
            .find(|session| is_playing(session))
            .or_else(|| manager.GetCurrentSession().ok());
        let Some(session) = session else {
            eprintln!("no active session");
            return;
        };
        let app = session.SourceAppUserModelId().unwrap_or_default();
        let op = match command.as_str() {
            "next" => session.TrySkipNextAsync(),
            "prev" => session.TrySkipPreviousAsync(),
            "play" => session.TryPlayAsync(),
            "pause" => session.TryPauseAsync(),
            "toggle" => session.TryTogglePlayPauseAsync(),
            other => {
                eprintln!("unknown command {other:?} (next|prev|play|pause|toggle)");
                return;
            }
        };
        let accepted = op.expect("start op").join().expect("join op");
        println!("app={app:?} command={command:?} accepted={accepted}");
        return;
    }

    dump_sessions(&manager);
}

fn is_playing(
    session: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
) -> bool {
    session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .is_ok_and(|status| status == PlaybackStatus::Playing)
}

fn dump_sessions(manager: &Manager) {
    let sessions = manager.GetSessions().expect("GetSessions");
    println!("== {} session(s) ==", sessions.Size().unwrap_or(0));
    for (i, session) in sessions.into_iter().enumerate() {
        let app_id = session.SourceAppUserModelId().unwrap_or_default();
        let status = session
            .GetPlaybackInfo()
            .ok()
            .and_then(|info| info.PlaybackStatus().ok())
            .map(|s| match s {
                PlaybackStatus::Playing => "Playing",
                PlaybackStatus::Paused => "Paused",
                PlaybackStatus::Stopped => "Stopped",
                PlaybackStatus::Changing => "Changing",
                _ => "Closed/Unknown",
            })
            .unwrap_or("?")
            .to_string();
        println!("[{i}] app={app_id:?} status={status}");

        if let Ok(op) = session.TryGetMediaPropertiesAsync() {
            if let Ok(props) = op.join() {
                println!(
                    "    Title       = {:?}",
                    props.Title().unwrap_or_default()
                );
                println!(
                    "    Artist      = {:?}",
                    props.Artist().unwrap_or_default()
                );
                println!(
                    "    Subtitle    = {:?}",
                    props.Subtitle().unwrap_or_default()
                );
                println!(
                    "    AlbumTitle  = {:?}",
                    props.AlbumTitle().unwrap_or_default()
                );
                println!(
                    "    AlbumArtist = {:?}",
                    props.AlbumArtist().unwrap_or_default()
                );
                println!(
                    "    TrackNumber = {:?}",
                    props.TrackNumber().unwrap_or_default()
                );
                let genres = props
                    .Genres()
                    .ok()
                    .map(|g| {
                        g.into_iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                println!("    Genres      = {genres:?}");
                let has_thumb = props.Thumbnail().is_ok();
                println!("    Thumbnail   = {has_thumb}");
            }
        }

        if let Ok(tl) = session.GetTimelineProperties() {
            println!(
                "    Timeline    = pos={:?}ms end={:?}ms",
                tl.Position().unwrap_or_default().Duration / 10_000,
                tl.EndTime().unwrap_or_default().Duration / 10_000
            );
        }
        println!();
    }
}
