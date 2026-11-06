import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import St from 'gi://St';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import * as fmt from './format.js';
import {focusSession} from './focus.js';
import {
    ContainerItem, ProcessItem, StatLine, actionItem, button, confirmButton, later, plainItem, syncKeyed,
} from './widgets.js';

/**
 * A submenu row whose header is a StatLine. Details are built when the row
 * opens (GNOME refuses to open an empty submenu) and refreshed only while it
 * stays open.
 */
const ExpandableRow = GObject.registerClass(
class HivetopExpandableRow extends PopupMenu.PopupSubMenuMenuItem {
    _init(ctx, lineOptions) {
        super._init('', false);
        this._ctx = ctx;
        this.data = null;
        this.label.hide();
        for (const child of this.get_children()) {
            if (child.has_style_class_name?.('popup-menu-item-expander'))
                child.hide();
        }
        this.line = new StatLine(lineOptions);
        this.insert_child_above(this.line.actor, this.label);
        this.details = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this.details);
        this._expandable = true;
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                later(this, 300, () => ctx.reveal(this, this.menu.actor));
        });
    }

    setSubmenuShown(open) {
        if (open && !this._expandable)
            return;
        if (open && this.data)
            this.renderDetails();
        super.setSubmenuShown(open);
    }

    /** Rows with nothing to show lose their chevron and do not open. */
    setExpandable(expandable) {
        this._expandable = expandable;
        if (this._triangleBin)
            this._triangleBin.opacity = expandable ? 255 : 0;
        if (!expandable && this.menu.isOpen)
            this.menu.close(false);
    }

    update(data) {
        this.data = data;
        this.renderLine();
        if (this.menu.isOpen)
            this.renderDetails();
    }

    renderLine() {}

    renderDetails() {}

    /** Containers (by id) then processes, reusing detail items across refreshes. */
    syncItems(containerIds, processes, {stoppable = false, killable = true} = {}) {
        this._items ??= new Map();
        const list = [
            ...containerIds.map(id => this._ctx.container(id))
                .filter(Boolean)
                .map(c => ({type: 'c', key: `c:${c.id}`, data: c})),
            ...processes.map(p => ({type: 'p', key: `p:${p.pid}:${p.start}`, data: p})),
        ];
        syncKeyed(this.details, this._items, list, it => it.key, it => {
            const item = it.type === 'c'
                ? new ContainerItem(this._ctx, {stoppable})
                : new ProcessItem(this._ctx, {killable});
            const update = item.update.bind(item);
            item.update = x => update(x.data);
            return item;
        });
    }
});

function sessionDetails(s) {
    return [
        `session: ${s.name}`,
        `session id: ${s.id}`,
        `pid: ${s.pid}`,
        `cwd: ${s.cwd}`,
        `entrypoint: ${s.entrypoint}`,
        `status: ${s.status}`,
        `running: ${s.activity ?? '-'}`,
        s.scratchpad ? `scratchpad: ${s.scratchpad}` : null,
        `cpu: ${fmt.pct(s.cpu)} of one core`,
        `memory: ${fmt.bytes(s.rss)} in ${s.procs} processes`,
        s.containers.length
            ? `containers: ${s.containers.length}, cpu ${fmt.pct(s.container_cpu)} of one core, memory ${fmt.bytes(s.container_mem)}`
            : null,
    ].filter(Boolean).join('\n');
}

export const SessionRow = GObject.registerClass(
class HivetopSessionRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {spark: true, status: true});
        this._goTo = button('Go to window', () => {
            if (focusSession(this.data))
                ctx.closeMenu();
            else
                ctx.notify('No window found for this session');
        });
        this._scratchpad = button('Scratchpad', () => {
            ctx.openFolder(this.data.scratchpad);
            ctx.closeMenu();
        });
        this.menu.addMenuItem(actionItem([
            this._goTo,
            this._scratchpad,
            button('Copy details', () => ctx.copy(sessionDetails(this.data))),
            confirmButton('Kill session', () => ctx.kill('session', this.data.key), 'hivetop-btn-danger'),
        ]), 0);
    }

    renderLine() {
        const s = this.data;
        let sub = s.activity ?? `${s.status} in ${fmt.homeShort(s.cwd, this._ctx.home)}`;
        if (s.containers.length)
            sub += `  + ${s.containers.length} container(s) ${fmt.bytes(s.container_mem)}`;
        this.line.set({title: s.name, sub, cpu: this._ctx.cpu(s.cpu), mem: fmt.bytes(s.rss)});
        this.line.setStatus(s.status === 'busy' ? 'busy' : 'idle');
        this.line.spark.setData(s.spark, {span: 600});
        this._goTo.label = s.app ? `Go to ${appName(s.app)}` : 'Go to window';
        this._scratchpad.visible = !!s.scratchpad;
    }

    renderDetails() {
        this.syncItems(this.data.containers, this.data.processes, {stoppable: true});
    }
});

export const OrphanRow = GObject.registerClass(
class HivetopOrphanRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {status: true});
        this.add_style_class_name('hivetop-orphan-row');
        this.menu.addMenuItem(actionItem([
            button('Copy details', () => ctx.copy([
                `ended session: ${this.data.name}`,
                `session id: ${this.data.session_id}`,
                `cwd: ${this.data.cwd}`,
                ...this.data.processes.map(p => `pid ${p.pid}: ${p.cmd}`),
                ...this.data.containers.map(c => `container: ${c}`),
            ].join('\n'))),
            confirmButton('Kill all leftovers', () => ctx.kill('orphans', this.data.session_id), 'hivetop-btn-danger'),
        ]), 0);
    }

    renderLine() {
        const o = this.data;
        const parts = [`ended ${fmt.ago(this._ctx.now() - o.ended_at)} ago`];
        if (o.procs)
            parts.push(`${o.procs} process(es)`);
        if (o.containers.length)
            parts.push(`${o.containers.length} container(s)`);
        this.line.set({title: o.name, sub: parts.join(', '), cpu: this._ctx.cpu(o.cpu), mem: fmt.bytes(o.rss)});
        this.line.setStatus('orphan');
    }

    renderDetails() {
        this.syncItems(this.data.containers, this.data.processes, {stoppable: true});
    }
});

const appCache = new Map();

function resolveApp(id) {
    if (appCache.has(id))
        return appCache.get(id);
    const sys = Shell.AppSystem.get_default();
    let app = null;
    for (const name of [`${id}.desktop`, `${id.toLowerCase()}.desktop`, `${id}_${id}.desktop`]) {
        app = sys.lookup_app(name);
        if (app)
            break;
    }
    app ??= sys.lookup_startup_wmclass(id) ?? sys.lookup_desktop_wmclass(id) ?? null;
    if (!app) {
        const best = Shell.AppSystem.search(id)?.[0]?.[0];
        app = best ? sys.lookup_app(best) : null;
    }
    appCache.set(id, app);
    return app;
}

export function appName(id) {
    return resolveApp(id)?.get_name() ?? id;
}

export function appKey(id) {
    return resolveApp(id)?.get_id() ?? id;
}

export function clearAppCache() {
    appCache.clear();
}

export const AppRow = GObject.registerClass(
class HivetopAppRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {icon: true});
    }

    renderLine() {
        const a = this.data;
        const app = resolveApp(a.ids[0]);
        if (!this.line.iconBin.child) {
            this.line.iconBin.child = app?.create_icon_texture(16)
                ?? new St.Icon({icon_name: 'application-x-executable-symbolic', icon_size: 16});
        }
        const parts = [`${a.procs} process${a.procs === 1 ? '' : 'es'}`];
        if (a.sessions)
            parts.push(`${a.sessions} Claude session${a.sessions === 1 ? '' : 's'} (${fmt.bytes(a.claude_mem)} above)`);
        else if (a.claude_mem)
            parts.push(`Claude ${fmt.bytes(a.claude_mem)} above`);
        if (a.adopted)
            parts.push(`${a.adopted} started outside its scope`);
        if (a.kind === 'session')
            parts.push('desktop session');
        else if (a.kind === 'background')
            parts.push('background service');
        this.line.set({title: app?.get_name() ?? a.ids[0], sub: parts.join(', '), cpu: this._ctx.cpu(a.cpu), mem: fmt.bytes(a.mem)});
        this.setExpandable(a.processes.length > 0);
    }

    renderDetails() {
        this.syncItems([], this.data.processes);
    }
});

export const StackRow = GObject.registerClass(
class HivetopStackRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {});
    }

    renderLine() {
        const g = this.data;
        this.setExpandable(g.containers.length > 0);
        this.line.set({
            title: g.project || 'containers',
            sub: `${g.containers.length} container(s)`,
            cpu: this._ctx.cpu(g.containers.reduce((n, c) => n + c.cpu, 0)),
            mem: fmt.bytes(g.containers.reduce((n, c) => n + c.mem, 0)),
        });
    }

    renderDetails() {
        this.syncItems(this.data.containers.map(c => c.id), []);
    }
});

/**
 * The last row of the Claude section: SDK sessions and shared tooling, as
 * non-expandable lines.
 */
export const BackgroundRow = GObject.registerClass(
class HivetopBackgroundRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {});
        this._lines = new Map();
    }

    renderLine() {
        const b = this.data;
        this.line.set({
            title: `Background (${b.items.length})`,
            sub: 'SDK sessions and plugin workers',
            cpu: this._ctx.cpu(b.cpu),
            mem: fmt.bytes(b.rss),
        });
    }

    renderDetails() {
        syncKeyed(this.details, this._lines, this.data.items, x => x.key,
            x => new LineItem({status: x.key !== 'tooling'}));
    }
});

/** A non-expandable line, used for background sessions and shared tooling. */
export const LineItem = GObject.registerClass(
class HivetopLineItem extends PopupMenu.PopupBaseMenuItem {
    _init(options = {}) {
        super._init({reactive: false, can_focus: false});
        this.line = new StatLine(options);
        this.add_child(this.line.actor);
    }

    update(data) {
        this.data = data;
        this.line.set(data);
        if (data.status)
            this.line.setStatus(data.status);
    }
});

export function systemItem() {
    const item = plainItem('hivetop-system');
    item.line = new StatLine({spark: true});
    item.add_child(item.line.actor);
    return item;
}
