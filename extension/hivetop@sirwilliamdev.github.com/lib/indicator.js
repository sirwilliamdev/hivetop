import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {Client, isCancelled} from './client.js';
import * as fmt from './format.js';
import {HistorySection} from './history.js';
import {AppRow, LineItem, OrphanRow, SessionRow, StackRow, appKey, clearAppCache, systemItem} from './rows.js';
import {SectionHeader, ensureVisible, later, syncKeyed} from './widgets.js';

const REFRESH_MS = 2000;
const PRESSURE_TEXT = ['calm', 'some pressure', 'high pressure'];

/**
 * A collapsible section: a header with totals, and rows matched by key across
 * refreshes. Rows are refreshed only while the section is expanded, and kept
 * (hidden) when it collapses: GNOME Shell retains memory for every destroyed
 * menu item, so rebuilding rows on each open would grow it.
 */
class KeyedSection {
    constructor(menu, title, reveal) {
        this.title = title;
        this.rows = new Map();
        this._last = null;
        this.header = new SectionHeader(expanded => {
            if (expanded) {
                this._syncRows();
                later(this.header, 50, () => reveal(this.header, this.section.actor));
            } else {
                this._closeRows();
            }
            this.section.actor.visible = expanded;
        });
        this.section = new PopupMenu.PopupMenuSection();
        menu.addMenuItem(this.header);
        menu.addMenuItem(this.section);
        this.header.visible = false;
        this.section.actor.visible = false;
    }

    /** `summary` is `{sub, cpu, mem}` for the header. */
    sync(items, keyOf, create, summary = {}) {
        this._last = {items, keyOf, create};
        this.header.line.set({title: `${this.title} (${items.length})`, ...summary});
        this.header.visible = items.length > 0;
        if (this.header.expanded)
            this._syncRows();
    }

    _syncRows() {
        if (this._last)
            syncKeyed(this.section, this.rows, this._last.items, this._last.keyOf, this._last.create);
    }

    _closeRows() {
        for (const row of this.rows.values()) {
            if (row.menu?.isOpen)
                row.menu.close(false);
        }
    }

    collapse() {
        this.header.setExpanded(false);
        this._closeRows();
        this.section.actor.visible = false;
    }
}

function metric(key) {
    const box = new St.BoxLayout({style_class: 'hivetop-metric', y_align: Clutter.ActorAlign.CENTER});
    const name = new St.Label({text: key, style_class: 'hivetop-metric-key', y_align: Clutter.ActorAlign.CENTER});
    const value = new St.Label({style_class: 'hivetop-metric-value', y_align: Clutter.ActorAlign.CENTER});
    box.add_child(name);
    box.add_child(value);
    return {box, value};
}

function sum(items, f) {
    return items.reduce((n, x) => n + f(x), 0);
}

export const Indicator = GObject.registerClass(
class HivetopIndicator extends PanelMenu.Button {
    _init(extension, settings) {
        super._init(0.0, 'hivetop', false);
        this._extension = extension;
        this._settings = settings;
        this._summary = null;
        this._snapshot = null;
        this._available = false;
        this._containers = new Map();
        this._timer = 0;
        this._busy = false;

        const box = new St.BoxLayout({style_class: 'panel-status-menu-box hivetop-panel'});
        this._bee = new St.Icon({
            gicon: Gio.icon_new_for_string(`${extension.path}/icons/hivetop-bee-symbolic.svg`),
            style_class: 'system-status-icon hivetop-bee',
        });
        this._cpu = metric('cpu');
        this._ram = metric('ram');
        for (const a of [this._bee, this._cpu.box, this._ram.box])
            box.add_child(a);
        this.add_child(box);

        this._client = new Client({
            onSummary: s => this._setSummary(s),
            onAvailability: up => this._setAvailable(up),
        });
        this._ctx = {
            home: GLib.get_home_dir(),
            client: this._client,
            now: () => this._snapshot?.ts ?? Math.floor(Date.now() / 1000),
            memTotal: () => this._summary?.mem_total ?? 0,
            // Per-core percent from the daemon -> share of the whole machine, as GNOME System Monitor shows it.
            cpu: perCore => fmt.share(perCore / Math.max(1, this._snapshot?.ncpu ?? 1)),
            container: id => this._containers.get(id),
            copy: text => St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text),
            notify: msg => Main.notify('hivetop', msg),
            closeMenu: () => this.menu.close(),
            kill: (kind, id) => this._kill(kind, id),
            reveal: (top, bottom) => ensureVisible(this._scroll, top, bottom),
            openFolder: path => {
                try {
                    Gio.AppInfo.launch_default_for_uri(GLib.filename_to_uri(path, null),
                        global.create_app_launch_context(0, -1));
                } catch (e) {
                    Main.notify('hivetop', `Cannot open ${path}: ${e.message}`);
                }
            },
        };

        this.menu.box.add_style_class_name('hivetop-menu');
        this._buildMenu();
        this.menu.connect('open-state-changed', (_menu, open) => this._onMenuToggled(open));
        this._settingsIds = ['show-cpu', 'show-ram', 'ram-as-percent', 'show-pressure']
            .map(k => this._settings.connect(`changed::${k}`, () => this._renderLabel()));
        this._renderLabel();
        this._client.snapshot(false)
            .then(s => this._setSummary(summaryOf(s)))
            .catch(e => {
                if (!isCancelled(e))
                    this._setAvailable(false);
            });
    }

    _buildMenu() {
        this._offline = new PopupMenu.PopupMenuItem('hivetopd is not running. Click to start it.');
        this._offline.connect('activate', () => {
            try {
                Gio.Subprocess.new(['systemctl', '--user', 'start', 'hivetopd.service'], Gio.SubprocessFlags.NONE);
            } catch (e) {
                Main.notify('hivetop', `Could not start hivetopd: ${e.message}`);
            }
        });
        this.menu.addMenuItem(this._offline);
        this._system = systemItem();
        this.menu.addMenuItem(this._system);
        // Sections scroll inside a capped view: a top-level menu taller than the
        // screen makes GNOME squeeze expanded submenus to zero height.
        const content = new PopupMenu.PopupMenuSection();
        // Not added through addMenuItem, so link it by hand: submenus inside
        // look up the top menu through this parent chain.
        content._setParent(this.menu);
        this._scroll = new St.ScrollView({
            style_class: 'hivetop-scroll',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: false,
            x_expand: true,
        });
        this._scroll.set_child(content.actor);
        const holder = new PopupMenu.PopupMenuSection();
        holder.actor.add_child(this._scroll);
        this.menu.addMenuItem(holder);
        const reveal = this._ctx.reveal;
        this._sessions = new KeyedSection(content, 'Claude sessions', reveal);
        this._orphans = new KeyedSection(content, 'Left behind by ended sessions', reveal);
        this._orphans.header.add_style_class_name('hivetop-warn-header');
        this._background = new KeyedSection(content, 'Background Claude', reveal);
        this._apps = new KeyedSection(content, 'Apps', reveal);
        this._stacks = new KeyedSection(content, 'Docker', reveal);
        this._history = new HistorySection(content, this._ctx);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        const prefs = new PopupMenu.PopupMenuItem('Settings');
        prefs.connect('activate', () => this._extension.openPreferences());
        this.menu.addMenuItem(prefs);
        this._syncOffline();
    }

    _sections() {
        return [this._sessions, this._orphans, this._background, this._apps, this._stacks, this._history];
    }

    _setAvailable(up) {
        this._available = up;
        if (!up)
            this._summary = null;
        this._renderLabel();
        this._syncOffline();
    }

    _syncOffline() {
        this._offline.visible = !this._available;
        this._system.visible = this._available;
    }

    _setSummary(summary) {
        this._summary = summary;
        this._available = true;
        this._syncOffline();
        this._renderLabel();
    }

    _renderLabel() {
        const s = this._summary;
        this._cpu.box.visible = !!s && this._settings.get_boolean('show-cpu');
        this._ram.box.visible = !!s && this._settings.get_boolean('show-ram');
        this._bee.opacity = s ? 255 : 128;
        for (const level of [1, 2])
            this._bee.remove_style_class_name(`hivetop-bee-${level}`);
        if (!s)
            return;
        this._cpu.value.text = fmt.pct(s.cpu);
        this._ram.value.text = this._settings.get_boolean('ram-as-percent')
            ? fmt.pct(100 * s.mem_used / Math.max(1, s.mem_total))
            : fmt.bytes(s.mem_used);
        const level = Math.min(2, s.pressure ?? 0);
        if (level > 0 && this._settings.get_boolean('show-pressure'))
            this._bee.add_style_class_name(`hivetop-bee-${level}`);
    }

    _onMenuToggled(open) {
        this._stopTimer();
        for (const section of this._sections())
            section.collapse();
        if (!open) {
            this._client.setActive(false).catch(() => {});
            return;
        }
        clearAppCache();
        const area = Main.layoutManager.getWorkAreaForMonitor(Main.layoutManager.primaryIndex);
        const scale = St.ThemeContext.get_for_stage(global.stage).scale_factor;
        this._scroll.style = `max-height: ${Math.floor(area.height * 0.7 / scale)}px;`;
        this._client.setActive(true)
            .catch(() => {})
            .finally(() => this._refresh());
        this._timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, REFRESH_MS, () => {
            this._refresh();
            return GLib.SOURCE_CONTINUE;
        });
    }

    _stopTimer() {
        if (this._timer)
            GLib.source_remove(this._timer);
        this._timer = 0;
    }

    async _refresh() {
        if (this._busy)
            return;
        this._busy = true;
        try {
            const snap = await this._client.snapshot(true);
            this._render(snap);
        } catch (e) {
            if (!isCancelled(e))
                this._setAvailable(false);
        } finally {
            this._busy = false;
        }
    }

    async _kill(kind, id) {
        try {
            const done = await this._client.kill(kind, id);
            if (!done)
                Main.notify('hivetop', 'Target is already gone.');
        } catch (e) {
            if (!isCancelled(e))
                Main.notify('hivetop', `Kill failed: ${e.message}`);
        }
        later(this, 600, () => {
            if (this.menu.isOpen)
                this._refresh();
        });
    }

    _render(snap) {
        this._snapshot = snap;
        this._setSummary(summaryOf(snap));
        this._containers = new Map(snap.containers.map(c => [c.id, c]));

        const sys = snap.system;
        const memPct = 100 * sys.mem_used / Math.max(1, sys.mem_total);
        const psi = sys.psi;
        this._system.line.set({
            title: `System: ${snap.ncpu} cores, ${PRESSURE_TEXT[Math.min(2, sys.pressure)]}`,
            sub: `RAM ${fmt.bytes(sys.mem_used)} / ${fmt.bytes(sys.mem_total)}, swap ${fmt.bytes(sys.swap_used)}. ` +
                `PSI cpu ${psi.cpu_some.toFixed(0)} mem ${psi.memory_some.toFixed(0)} io ${psi.io_some.toFixed(0)}`,
            cpu: fmt.pct(sys.cpu),
            mem: fmt.pct(memPct),
        });
        this._system.line.spark.setData(sys.spark, {span: 600, cpuMax: 100, memMax: 100});

        const interactive = snap.sessions.filter(s => !s.background);
        const busy = interactive.filter(s => s.status === 'busy').length;
        this._sessions.sync(interactive, s => s.key, () => new SessionRow(this._ctx), {
            sub: `${busy} busy, ${interactive.length - busy} idle`,
            cpu: this._ctx.cpu(sum(interactive, s => s.cpu + s.container_cpu)),
            mem: fmt.bytes(sum(interactive, s => s.rss + s.container_mem)),
        });

        this._orphans.sync(snap.orphans, o => o.session_id, () => new OrphanRow(this._ctx), {
            sub: `${sum(snap.orphans, o => o.procs)} process(es), ${sum(snap.orphans, o => o.containers.length)} container(s) still running`,
            cpu: this._ctx.cpu(sum(snap.orphans, o => o.cpu)),
            mem: fmt.bytes(sum(snap.orphans, o => o.rss)),
        });

        const background = snap.sessions.filter(s => s.background).map(s => ({
            key: s.key,
            title: s.name,
            sub: `${s.entrypoint} in ${fmt.homeShort(s.cwd, this._ctx.home)}`,
            cpu: this._ctx.cpu(s.cpu),
            mem: fmt.bytes(s.rss),
            status: s.status === 'busy' ? 'busy' : 'idle',
            rss: s.rss,
            cpuValue: s.cpu,
        }));
        if (snap.tooling.procs > 0) {
            background.push({
                key: 'tooling',
                title: 'Shared Claude tooling',
                sub: `${snap.tooling.procs} process(es) outside any session, e.g. plugin workers`,
                cpu: this._ctx.cpu(snap.tooling.cpu),
                mem: fmt.bytes(snap.tooling.rss),
                rss: snap.tooling.rss,
                cpuValue: snap.tooling.cpu,
            });
        }
        this._background.sync(background, b => b.key, b => new LineItem({status: b.key !== 'tooling'}), {
            sub: 'SDK sessions and plugin workers',
            cpu: this._ctx.cpu(sum(background, b => b.cpuValue)),
            mem: fmt.bytes(sum(background, b => b.rss)),
        });

        const merged = new Map();
        for (const a of snap.apps) {
            const key = appKey(a.id);
            const m = merged.get(key);
            if (m) {
                m.ids.push(a.id);
                m.mem += a.mem;
                m.cpu += a.cpu;
                m.sessions += a.sessions;
                m.procs += a.procs;
                m.adopted += a.adopted;
                m.processes = [...m.processes, ...a.processes].sort((x, y) => y.rss - x.rss).slice(0, 8);
            } else {
                merged.set(key, {key, ids: [a.id], ...a});
            }
        }
        const apps = [...merged.values()];
        this._apps.sync(apps, a => a.key, () => new AppRow(this._ctx), {
            sub: 'largest apps by memory, Claude sessions included',
            cpu: this._ctx.cpu(sum(apps, a => a.cpu)),
            mem: fmt.bytes(sum(apps, a => a.mem)),
        });

        const stacks = new Map();
        const loose = snap.containers.filter(x => !x.session && !x.orphan_of);
        for (const c of loose) {
            const key = c.project || c.name;
            if (!stacks.has(key))
                stacks.set(key, {project: c.project, containers: []});
            stacks.get(key).containers.push(c);
        }
        this._stacks.sync([...stacks.values()], g => g.project || g.containers[0].name, () => new StackRow(this._ctx), {
            sub: snap.docker_ok ? 'containers not started by a Claude session' : 'Docker not reachable',
            cpu: this._ctx.cpu(sum(loose, c => c.cpu)),
            mem: fmt.bytes(sum(loose, c => c.mem)),
        });
    }

    destroy() {
        this._stopTimer();
        for (const id of this._settingsIds ?? [])
            this._settings.disconnect(id);
        this._settingsIds = null;
        this._client?.destroy();
        this._client = null;
        super.destroy();
    }
});

function summaryOf(snap) {
    return {
        ts: snap.ts,
        cpu: snap.system.cpu,
        mem_used: snap.system.mem_used,
        mem_total: snap.system.mem_total,
        pressure: snap.system.pressure,
        sessions: snap.sessions.filter(s => !s.background).length,
        orphans: snap.orphans.length,
    };
}
