import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';
import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const POSITIONS = ['left', 'center', 'right'];

const CONFIG_TEMPLATE = `# hivetopd configuration. Saved changes are picked up within seconds.

# Sampling interval while the menu is open / closed, in milliseconds.
interval_active_ms = 2000
interval_idle_ms = 5000

[notify]
enabled = true
session_rss_mb = 4096      # warn when one AI agent session uses more RAM than this
session_cpu_pct = 300.0    # ... or more CPU than this (100 = one core)
session_cpu_secs = 60      # ... for at least this long
psi_memory_some = 20.0     # warn on memory pressure (PSI some avg10, percent)
orphans = true             # warn when an ended session leaves work running
cooldown_secs = 600

[history]
enabled = true
rollup_secs = 60
retention_days = 30
downsample_after_days = 7
top_apps = 5

[docker]
enabled = true
# socket = "/var/run/docker.sock"
refresh_secs = 30

[agents.claude]
enabled = true
# config_dir = "~/.claude"      # default: $CLAUDE_CONFIG_DIR or ~/.claude
# tmp_dir = "/tmp/claude-1000"  # default: /tmp/claude-<uid>
`;

function configFile() {
    const dir = GLib.build_filenamev([GLib.get_user_config_dir(), 'hivetop']);
    return Gio.File.new_for_path(GLib.build_filenamev([dir, 'config.toml']));
}

export default class HivetopPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        window._settings = settings;
        const page = new Adw.PreferencesPage({title: 'hivetop', icon_name: 'utilities-system-monitor-symbolic'});

        const bar = new Adw.PreferencesGroup({title: 'Top bar'});
        const switches = [
            ['show-cpu', 'Show CPU', 'Total CPU usage across all cores'],
            ['show-ram', 'Show RAM', 'Memory used, without reclaimable cache'],
            ['ram-as-percent', 'RAM as percent', 'Off shows used GB instead'],
            ['show-pressure', 'Pressure color', 'Bee turns yellow or red when the kernel reports CPU, memory or IO pressure'],
        ];
        for (const [key, title, subtitle] of switches) {
            const row = new Adw.SwitchRow({title, subtitle});
            settings.bind(key, row, 'active', Gio.SettingsBindFlags.DEFAULT);
            bar.add(row);
        }
        const position = new Adw.ComboRow({
            title: 'Area',
            model: Gtk.StringList.new(['Left', 'Center', 'Right']),
            selected: Math.max(0, POSITIONS.indexOf(settings.get_string('panel-position'))),
        });
        position.connect('notify::selected', () =>
            settings.set_string('panel-position', POSITIONS[position.selected]));
        bar.add(position);
        const index = new Adw.SpinRow({
            title: 'Position in area',
            adjustment: new Gtk.Adjustment({lower: 0, upper: 20, step_increment: 1}),
        });
        settings.bind('panel-index', index, 'value', Gio.SettingsBindFlags.DEFAULT);
        bar.add(index);
        page.add(bar);

        const alerts = new Adw.PreferencesGroup({title: 'Notifications'});
        const waiting = new Adw.SwitchRow({
            title: 'When an AI agent needs you',
            subtitle: 'Once per wait: a permission dialog, a question, a blocked background job. Click it to go to the agent',
        });
        settings.bind('notify-waiting', waiting, 'active', Gio.SettingsBindFlags.DEFAULT);
        alerts.add(waiting);
        page.add(alerts);

        const daemon = new Adw.PreferencesGroup({
            title: 'Daemon',
            description: 'Thresholds, history and Docker settings live in the hivetopd config file.',
        });
        const open = new Adw.ActionRow({title: 'Edit config file', subtitle: configFile().get_path(), activatable: true});
        open.add_suffix(new Gtk.Image({icon_name: 'document-edit-symbolic'}));
        open.connect('activated', () => {
            const file = configFile();
            try {
                if (!file.query_exists(null)) {
                    file.get_parent().make_directory_with_parents(null);
                    file.replace_contents(new TextEncoder().encode(CONFIG_TEMPLATE), null, false,
                        Gio.FileCreateFlags.NONE, null);
                }
                Gio.AppInfo.launch_default_for_uri(file.get_uri(), null);
            } catch (e) {
                console.error(`hivetop: cannot open config: ${e}`);
            }
        });
        daemon.add(open);
        page.add(daemon);

        window.add(page);
    }
}
