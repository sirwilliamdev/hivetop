import * as Main from 'resource:///org/gnome/shell/ui/main.js';

/**
 * Raises the window that hosts a session. Walks the session's ancestor PIDs to
 * the first one owning a window. When one process owns several windows (one
 * VSCodium instance, many windows), the window whose title names the project
 * wins; otherwise the most recently used one.
 */
export function focusSession(session) {
    const chain = [session.pid, ...(session.ancestors ?? [])];
    const windows = global.get_window_actors()
        .map(a => a.meta_window)
        .filter(w => w && !w.is_skip_taskbar());
    const hint = (session.project ?? '').toLowerCase();
    for (const pid of chain) {
        const owned = windows.filter(w => w.get_pid() === pid);
        if (owned.length === 0)
            continue;
        const byTitle = hint
            ? owned.find(w => (w.get_title() ?? '').toLowerCase().includes(hint))
            : null;
        const target = byTitle ?? owned.sort((a, b) => b.get_user_time() - a.get_user_time())[0];
        Main.activateWindow(target);
        return true;
    }
    return false;
}
