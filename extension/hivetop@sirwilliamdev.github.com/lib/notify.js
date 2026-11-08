import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as MessageTray from 'resource:///org/gnome/shell/ui/messageTray.js';
import {PACKAGE_VERSION} from 'resource:///org/gnome/shell/misc/config.js';

import {focusSession} from './focus.js';

const SHELL_MAJOR = parseInt(PACKAGE_VERSION, 10);
const ICON = 'utilities-system-monitor-symbolic';

/**
 * Notifies when an AI agent starts waiting for a person, once per wait: an
 * agent is notified again only after it stopped waiting. Clicking the
 * notification raises the agent's window. Off unless `notify-waiting` is set.
 */
export class WaitNotifier {
    constructor(settings, client) {
        this._settings = settings;
        this._client = client;
        this._seen = null;
        this._source = null;
    }

    /** `waiting` is the summary's list of `{key, title, reason, since}`. */
    update(waiting) {
        waiting ??= [];
        const current = new Map(waiting.map(w => [w.key, w.since]));
        // The first summary only records who already waits: no burst of
        // notifications at login.
        if (this._seen && this._settings.get_boolean('notify-waiting')) {
            for (const w of waiting) {
                if (this._seen.get(w.key) !== w.since)
                    this._show(w);
            }
        }
        this._seen = current;
    }

    _show(w) {
        const title = `${w.title} needs you`;
        const body = w.reason ? `Waiting: ${w.reason}` : 'Waiting for your answer';
        if (SHELL_MAJOR < 46) {
            Main.notify(title, body);
            return;
        }
        if (!this._source) {
            this._source = new MessageTray.Source({title: 'hivetop', iconName: ICON});
            this._source.connect('destroy', () => {
                this._source = null;
            });
            Main.messageTray.add(this._source);
        }
        const n = new MessageTray.Notification({source: this._source, title, body});
        n.connect('activated', () => this._focus(w.key));
        this._source.addNotification(n);
    }

    async _focus(key) {
        try {
            const snap = await this._client.snapshot(false);
            const agent = snap.agents.find(a => a.key === key);
            if (agent)
                focusSession(agent);
        } catch (e) {
            console.warn(`hivetop: cannot raise agent window: ${e}`);
        }
    }

    destroy() {
        this._source?.destroy();
        this._source = null;
        this._client = null;
    }
}
