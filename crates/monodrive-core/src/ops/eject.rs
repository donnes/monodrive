//! Eject, as Finder's does, refused while a Monodrive job still has work on
//! a volume it takes along.

use super::{Client, State};
use crate::Origin;
use crate::volumes::{self, Volume};
use std::path::Path;

impl Client {
    /// Ejects the volume mounted at `mount`, as Finder's Eject does: the whole
    /// disk when its media can be ejected or unplugged, otherwise only this
    /// volume. Refuses the startup disk and a volume a Monodrive job still has
    /// work on, and passes on macOS's refusal, such as a busy volume. Never
    /// forces. Waits for macOS, so call it off the UI thread.
    pub fn eject(&mut self, mount: &Path, origin: &Origin) -> Result<String, String> {
        if matches!(origin, Origin::Agent { .. }) && !self.agents_change_files() {
            return Err(
                "Agents can't eject volumes: Let agents change files is off in Monodrive's Agents settings."
                    .into(),
            );
        }
        // Hidden ones too: a whole-disk eject takes them along, jobs and all.
        let volumes = volumes::mounted(true);
        let volume = volumes
            .iter()
            .find(|volume| volume.mount == mount)
            .ok_or_else(|| format!("{} isn't a mounted volume.", mount.display()))?;
        self.ready(volume, &volumes)?;
        volumes::unmount(volume)?;
        Ok(format!("Ejected {}.", volume.name))
    }

    /// Whether `volume` may be ejected now: neither it nor another volume
    /// ejecting it takes along is the startup disk, and no job queued, running
    /// or waiting at a conflict reads or writes any of them.
    fn ready(&mut self, volume: &Volume, volumes: &[Volume]) -> Result<(), String> {
        let leaving = volumes::ejected_with(volume, volumes);
        // Why one of them can't go, said of the volume asked about.
        let refuse = |going: &Volume, why: &str| {
            if going.mount == volume.mount {
                format!("{} is {why}", going.name)
            } else {
                format!("Ejecting {} also ejects {}, {why}", volume.name, going.name)
            }
        };
        if let Some(startup) = leaving.iter().find(|going| going.startup) {
            return Err(refuse(startup, "the startup disk."));
        }
        let jobs = self.jobs()?;
        let active = jobs
            .iter()
            .filter(|job| matches!(job.state, State::Queued | State::Running | State::Waiting));
        for job in active {
            let touches = |mount: &Path| {
                job.into.starts_with(mount)
                    || job.tasks.iter().any(|task| task.source.starts_with(mount))
            };
            if let Some(going) = leaving.iter().find(|going| touches(&going.mount)) {
                let which = if going.mount == volume.mount {
                    ""
                } else {
                    "which is "
                };
                let why = format!(
                    "{which}in use by Monodrive's job {} ({} into {}, {}). Let it finish or cancel it first.",
                    job.id,
                    job.title,
                    job.into.display(),
                    job.state.label()
                );
                return Err(refuse(going, &why));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Conflict, Owner, Paths, Request};
    use super::*;

    #[test]
    fn eject_refuses_the_startup_disk_and_a_volume_a_job_still_has_work_on() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            state: dir.path().join("state"),
            settings: dir.path().join("settings.json"),
            trash: dir.path().join("Trash"),
        };
        let (file, into) = (dir.path().join("a.txt"), dir.path().join("into"));
        std::fs::write(&file, "a").unwrap();
        std::fs::create_dir(&into).unwrap();
        std::fs::write(into.join("a.txt"), "taken").unwrap();
        // The copy stops at the taken name and waits; the owner exits once
        // it has, and the next one reads the job back from its journal.
        let owner = Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut client = Client::new(paths.clone(), None);
        let copy = Request::Copy {
            paths: vec![file],
            into: into.clone(),
            conflict: Conflict::Stop,
        };
        client.submit(Origin::Window, copy).unwrap();
        drop(client);
        serving.join().unwrap().unwrap();

        let owner = Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut client = Client::new(paths, None);
        let volume = |name: &str, mount: &Path, disk: u64, ejectable| Volume {
            name: name.into(),
            mount: mount.into(),
            disk: Some(disk),
            id: None,
            total: 0,
            free: 0,
            read_only: false,
            network: false,
            ejectable,
            removable: false,
            startup: mount == Path::new("/"),
            system: None,
        };
        let volumes = [
            volume("Macintosh HD", "/".as_ref(), 0, false),
            volume("Scratch", "/Volumes/Scratch".as_ref(), 0, false),
            volume("Backup", "/Volumes/Backup".as_ref(), 4, true),
            volume("Photos", dir.path(), 4, true),
            volume("Other", "/Volumes/Other".as_ref(), 5, true),
        ];
        let check = |name: &str, client: &mut Client| {
            let volume = volumes.iter().find(|v| v.name == name).unwrap();
            client.ready(volume, &volumes)
        };
        let busy = check("Photos", &mut client).unwrap_err();
        assert!(
            busy.starts_with("Photos is in use by Monodrive's job 1 (Copy into "),
            "{busy}"
        );
        assert!(busy.contains("waiting"), "{busy}");
        // Ejecting a disk unmounts all its volumes, so each one counts.
        let sibling = check("Backup", &mut client).unwrap_err();
        assert!(
            sibling.starts_with(
                "Ejecting Backup also ejects Photos, which is in use by Monodrive's job 1"
            ),
            "{sibling}"
        );
        assert_eq!(check("Other", &mut client), Ok(()));
        // A volume that isn't ejectable is only unmounted, alone.
        assert_eq!(check("Scratch", &mut client), Ok(()));
        assert_eq!(
            check("Macintosh HD", &mut client),
            Err("Macintosh HD is the startup disk.".into())
        );
        let booted_from_external = [
            volume("External", "/".as_ref(), 7, true),
            volume("Extra", "/Volumes/Extra".as_ref(), 7, true),
        ];
        assert_eq!(
            client.ready(&booted_from_external[1], &booted_from_external),
            Err("Ejecting Extra also ejects External, the startup disk.".into())
        );
        drop(client);
        serving.join().unwrap().unwrap();
    }
}
