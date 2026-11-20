// SPDX-License-Identifier: GPL-3.0-or-later
use std::{env, fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn mic_off_filters_the_config_file_and_the_environment_override() {
    const EXPECTED: &str = "VOICE_CLIPPER_TEST_EXPECTED_TRACKS";
    if let Ok(expected) = env::var(EXPECTED) {
        assert_eq!(
            clipper::gsr::audio_list(),
            expected.lines().collect::<Vec<_>>()
        );
        return;
    }

    // Run the real configuration path in a child so parallel tests never share
    // environment variables. The two commands only enumerate synthetic devices.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for subdir in ["config/clipper", "state/clipper", "bin"] {
        fs::create_dir_all(dir.join(subdir)).unwrap();
    }
    for (name, arg, text) in [
        (
            "pactl",
            "--format=json list sources",
            r#"[{"name":"studio_mic","monitor_source":""},{"name":"speakers_copy","monitor_source":"speakers"}]"#,
        ),
        (
            "gpu-screen-recorder",
            "--list-audio-devices",
            "studio_mic|Studio mic\nspeakers_copy|Monitor",
        ),
    ] {
        let path = dir.join("bin").join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$*\" = '{arg}' ] || exit 1\nprintf '%s\\n' '{text}'\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(dir.join("state/clipper/gsr_no_mic"), b"").unwrap();
    fs::write(
        dir.join("config/clipper/gsr_audio.txt"),
        "studio_mic\ndevice:speakers_copy\ndefault_output\napp:game|studio_mic\napp-inverse:chat\ndevice:studio_mic\n",
    )
    .unwrap();

    for (setting, expected) in [
        (
            None,
            "device:speakers_copy|default_output\ndevice:speakers_copy\ndefault_output\napp:game\napp-inverse:chat",
        ),
        (
            Some("device:studio_mic|app-inverse:override|default_output|device:speakers_copy"),
            "default_output|device:speakers_copy\napp-inverse:override\ndefault_output\ndevice:speakers_copy",
        ),
    ] {
        let mut child = Command::new(env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "mic_off_filters_the_config_file_and_the_environment_override",
                "--nocapture",
            ])
            .env(EXPECTED, expected)
            .env("PATH", dir.join("bin"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env_remove("CLIPPER_GSR_AUDIO");
        if let Some(setting) = setting {
            child.env("CLIPPER_GSR_AUDIO", setting);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
