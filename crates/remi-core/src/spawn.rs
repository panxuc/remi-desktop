//! Starting child processes without announcing them to the user.

use std::process::Command;

/// Keeps a console window from appearing when this command runs.
///
/// Everything the pet spawns — `ssh`, and whatever installing to a remote comes to need — is a
/// **console** program, and the pet itself is a GUI one with no console of its own. Windows
/// gives a console child a console, and a console has a window: without this, every connection
/// and every retry against a host that is switched off flashes a terminal in the user's face.
/// `CREATE_NO_WINDOW` says the child is a console program being run without one, which leaves
/// its stdin, stdout and stderr to the pipes we set and takes only the window away.
///
/// ⚠️ The symptom only appears in a **release** build. A debug build is not
/// `windows_subsystem = "windows"`, so it owns a console, the child inherits it, and nothing
/// pops up — which makes this exactly the kind of fix that can look verified against the wrong
/// binary.
///
/// Elsewhere there is no window to suppress and this does nothing.
pub fn without_a_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        /// From `processthreadsapi.h`. Named here rather than pulled in as a dependency: one
        /// constant does not earn a crate, and it is part of the Win32 ABI, so it cannot change.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        // Sets the flags rather than adding to them — this is the only thing in the workspace
        // that touches them.
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // `command` is otherwise untouched, and named so on every platform for the doc link.
    let _ = command;
}
