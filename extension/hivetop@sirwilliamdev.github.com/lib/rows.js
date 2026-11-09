import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import Shell from 'gi://Shell';
import St from 'gi://St';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import * as fmt from './format.js';
import {focusSession} from './focus.js';
import {
    ContainerItem, ProcessItem, StatLine, actionItem, button, confirmButton, ellipsized, later, plainItem, syncKeyed,
    vbox,
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

const STATE_WORD = {busy: 'busy', idle: 'idle', waiting: 'needs you'};
const PROVIDER_LABEL = {claude: 'Claude'};

export function providerLabel(id) {
    return PROVIDER_LABEL[id] ?? id;
}

function plural(n, word) {
    return `${n} ${word}${n === 1 ? '' : 's'}`;
}

function runningSubagents(a) {
    return (a.subagents ?? []).filter(x => x.running).length;
}

function modelLine(a) {
    return [a.model, a.mode ? `${a.mode} mode` : null, a.branch ? `branch ${a.branch}` : null]
        .filter(Boolean).join(' \u00b7 ');
}

function tokenLine(a) {
    const t = a.tokens ?? {};
    const own = (t.input ?? 0) + (t.cache_write ?? 0);
    return `${fmt.tokens(t.total)}  (in ${fmt.tokens(own)}, out ${fmt.tokens(t.output)}, subagents included)`;
}

function agentDetails(a) {
    const o = a.outputs ?? {};
    return [
        `agent: ${a.title}`,
        `provider: ${providerLabel(a.provider)} (${a.kind})`,
        `session: ${a.name}`,
        `session id: ${a.id}`,
        a.job ? `job: ${a.job.id} (${a.job.state})${a.job.detail ? `: ${a.job.detail}` : ''}` : null,
        `pid: ${a.pid}`,
        `cwd: ${a.cwd}`,
        `state: ${a.state}${a.reason ? ` (${a.reason})` : ''}`,
        a.model ? `model: ${modelLine(a)}` : null,
        `context: ${fmt.tokens(a.context)} tokens`,
        `tokens: ${tokenLine(a)}, cache reads ${fmt.tokens(a.tokens?.cache_read)}`,
        `produced: ${fmt.produced(o) || 'answer only'}`,
        ...(o.changes ?? []).map(c => `${fmt.change(c)}: ${c.url}`),
        ...(a.subagents ?? []).map(x => `subagent: ${x.kind}, ${x.description} (${x.running ? 'running' : 'done'})`),
        `running: ${a.activity ?? '-'}`,
        a.scratchpad ? `scratchpad: ${a.scratchpad}` : null,
        `cpu: ${fmt.pct(a.cpu)} of one core`,
        `memory: ${fmt.bytes(a.rss)} in ${a.procs} processes${a.detached ? ` (${a.detached} detached)` : ''}`,
        a.containers.length
            ? `containers: ${a.containers.length}, cpu ${fmt.pct(a.container_cpu)} of one core, memory ${fmt.bytes(a.container_mem)}`
            : null,
    ].filter(Boolean).join('\n');
}

/** Model, context, tokens and outputs of an agent, as label rows. */
const InfoItem = GObject.registerClass(
class HivetopInfoItem extends PopupMenu.PopupBaseMenuItem {
    _init() {
        super._init({reactive: false, can_focus: false, style_class: 'hivetop-info-item'});
        this._box = vbox({style_class: 'hivetop-kv', x_expand: true});
        this.add_child(this._box);
        this._rows = new Map();
        for (const key of ['Job', 'Model', 'Context', 'Tokens', 'Cache reads', 'Outputs']) {
            const row = new St.BoxLayout({style_class: 'hivetop-kv-row'});
            const k = new St.Label({text: key, style_class: 'hivetop-kv-key'});
            const v = new St.Label({style_class: 'hivetop-kv-value', x_expand: true});
            v.clutter_text.line_wrap = true;
            v.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
            v.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
            row.add_child(k);
            row.add_child(v);
            this._box.add_child(row);
            this._rows.set(key, {row, v});
        }
    }

    _set(key, text) {
        const r = this._rows.get(key);
        r.v.text = text ?? '';
        r.row.visible = !!text;
    }

    update(a) {
        this._set('Job', a.job ? [a.job.state, a.job.detail].filter(Boolean).join(': ') : '');
        this._set('Model', modelLine(a));
        this._set('Context', a.context ? `${fmt.tokens(a.context)} tokens` : '');
        this._set('Tokens', a.tokens?.total ? tokenLine(a) : '');
        this._set('Cache reads', a.tokens?.cache_read ? `${fmt.tokens(a.tokens.cache_read)}, not in the total` : '');
        this._set('Outputs', a.model || a.tokens?.total ? fmt.produced(a.outputs) || 'answer only' : '');
        this.visible = [...this._rows.values()].some(r => r.row.visible);
    }
});

/** The tinted panel of an agent's subagents: running first, then the last finished. */
const SubagentsItem = GObject.registerClass(
class HivetopSubagentsItem extends PopupMenu.PopupBaseMenuItem {
    _init(ctx) {
        super._init({reactive: false, can_focus: false, style_class: 'hivetop-subagents-item'});
        this._ctx = ctx;
        this._panel = vbox({style_class: 'hivetop-subagents', x_expand: true});
        const head = new St.BoxLayout({style_class: 'hivetop-sa-head'});
        head.add_child(new St.Label({text: 'SUBAGENTS', x_expand: true}));
        this._summary = new St.Label({x_align: Clutter.ActorAlign.END});
        head.add_child(this._summary);
        this._panel.add_child(head);
        this._lines = vbox();
        this._panel.add_child(this._lines);
        this.add_child(this._panel);
        this._cache = new Map();
    }

    _line(x) {
        let l = this._cache.get(x.id);
        if (!l) {
            const box = new St.BoxLayout({style_class: 'hivetop-sa-line'});
            const dot = new St.Widget({style_class: 'hivetop-status', y_align: Clutter.ActorAlign.CENTER});
            const type = new St.Label({style_class: 'hivetop-sa-type', y_align: Clutter.ActorAlign.CENTER});
            const desc = ellipsized('hivetop-sa-desc');
            desc.x_expand = true;
            const meta = new St.Label({style_class: 'hivetop-sa-meta', y_align: Clutter.ActorAlign.CENTER});
            for (const w of [dot, type, desc, meta])
                box.add_child(w);
            l = {box, dot, type, desc, meta};
            this._cache.set(x.id, l);
            this._lines.add_child(box);
        }
        return l;
    }

    update(list) {
        list ??= [];
        this.visible = list.length > 0;
        const running = list.filter(x => x.running).length;
        this._summary.text = [running ? `${running} running` : null,
            list.length - running ? `${list.length - running} done` : null].filter(Boolean).join(', ');
        const seen = new Set();
        const now = this._ctx.now();
        for (const x of list) {
            seen.add(x.id);
            const l = this._line(x);
            l.dot.style_class = `hivetop-status hivetop-status-${x.running ? 'busy' : 'idle'}`;
            l.type.text = x.kind || 'agent';
            l.desc.text = x.description || x.id;
            if (x.running)
                l.desc.remove_style_class_name('hivetop-sa-done');
            else
                l.desc.add_style_class_name('hivetop-sa-done');
            const when = x.running
                ? `running ${fmt.ago(now - x.started_at)}`
                : `${fmt.ago(now - (x.ended_at ?? now))} ago`;
            l.meta.text = `${when} \u00b7 ${fmt.tokens(x.tokens)}`;
        }
        for (const [id, l] of this._cache) {
            if (!seen.has(id)) {
                l.box.destroy();
                this._cache.delete(id);
            }
        }
        for (const x of list)
            this._lines.set_child_above_sibling(this._cache.get(x.id).box, null);
    }
});

/** One agent session: title, state, context, tokens, what it runs or produced. */
export const AgentRow = GObject.registerClass(
class HivetopAgentRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {spark: true, status: true, sub2: true});
        this._goTo = button('Go to window', () => {
            if (focusSession(this.data))
                ctx.closeMenu();
            else
                ctx.notify('No window found for this session');
        });
        this._attach = button('Attach', () => {
            const cmd = `claude attach ${this.data.job.id}`;
            ctx.copy(cmd);
            ctx.notify(`Copied "${cmd}": paste it in a terminal`);
        });
        this._scratchpad = button('Scratchpad', () => {
            ctx.openFolder(this.data.scratchpad);
            ctx.closeMenu();
        });
        this._kill = confirmButton('Kill session', () => ctx.kill('agent', this.data.key), 'hivetop-btn-danger');
        this._stop = confirmButton('Stop job', () => ctx.kill('job', this.data.job.id), 'hivetop-btn-danger');
        this.menu.addMenuItem(actionItem([
            this._goTo,
            this._attach,
            this._scratchpad,
            button('Copy details', () => ctx.copy(agentDetails(this.data))),
            this._kill,
            this._stop,
        ]), 0);
        this._info = new InfoItem();
        this.menu.addMenuItem(this._info, 1);
        this._subagents = new SubagentsItem(ctx);
        this.menu.addMenuItem(this._subagents, 2);
    }

    renderLine() {
        const a = this.data;
        const now = this._ctx.now();
        const running = runningSubagents(a);
        const sub = [
            a.project,
            `${STATE_WORD[a.state] ?? a.state} ${fmt.ago(now - a.state_since)}`,
            a.context ? `ctx ${fmt.tokens(a.context)}` : null,
            a.tokens?.total ? `${fmt.tokens(a.tokens.total)} tok` : null,
            running ? plural(running, 'subagent') : null,
        ].filter(Boolean).join(' \u00b7 ');
        let sub2 = '';
        let variant = 'out';
        if (a.state === 'waiting') {
            sub2 = `Waiting: ${a.reason ?? 'your answer'}`;
            variant = 'wait';
        } else if (a.activity) {
            sub2 = a.activity;
            variant = 'cmd';
        } else if (a.job?.detail) {
            sub2 = a.job.detail;
        } else {
            const out = fmt.produced(a.outputs);
            sub2 = out ? `Produced: ${out}` : '';
        }
        const extra = [];
        if (a.detached)
            extra.push(`+ ${a.detached} detached`);
        if (a.containers.length)
            extra.push(`+ ${a.containers.length} container(s) ${fmt.bytes(a.container_mem)}`);
        if (extra.length)
            sub2 = [sub2, ...extra].filter(Boolean).join(' \u00b7 ');
        const chip = [a.kind === 'job' ? 'bg' : null, a.tag].filter(Boolean).join(' ');
        this.line.set({title: a.title, sub, chip, cpu: this._ctx.cpu(a.cpu), mem: fmt.bytes(a.rss)});
        this.line.setSub2(sub2, variant);
        this.line.setStatus(a.state === 'waiting' ? 'wait' : a.state === 'busy' ? 'busy' : 'idle');
        this.line.spark.setData(a.spark, {span: 600});
        const job = a.kind === 'job' && a.job;
        this._goTo.visible = !job;
        this._attach.visible = !!job;
        this._kill.visible = !job;
        this._stop.visible = !!job;
        this._goTo.label = a.app ? `Go to ${appName(a.app)}` : 'Go to window';
        this._scratchpad.visible = !!a.scratchpad;
    }

    renderDetails() {
        this._info.update(this.data);
        this._subagents.update(this.data.subagents);
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
        this.line.set({title: `Left behind: ${o.name}`, sub: parts.join(', '), cpu: this._ctx.cpu(o.cpu), mem: fmt.bytes(o.rss)});
        this.line.title.add_style_class_name('hivetop-title-warn');
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
        if (a.agents)
            parts.push(`${plural(a.agents, 'AI agent')} (${fmt.bytes(a.agent_mem)} counted above)`);
        else if (a.agent_mem)
            parts.push(`AI agents ${fmt.bytes(a.agent_mem)} counted above`);
        if (a.adopted)
            parts.push(`${a.adopted} started outside its scope`);
        if (a.kind === 'session')
            parts.push('desktop session');
        else if (a.kind === 'background')
            parts.push('background service');
        else if (!a.windowed)
            parts.push('no open window');
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
 * The last row of the AI Agents section: SDK sessions and each provider's
 * tooling, as non-expandable lines.
 */
export const BackgroundRow = GObject.registerClass(
class HivetopBackgroundRow extends ExpandableRow {
    _init(ctx) {
        super._init(ctx, {status: true});
        // Keeps the title in line with the agent rows above.
        this.line.dot.opacity = 0;
        this._lines = new Map();
    }

    renderLine() {
        const b = this.data;
        this.line.set({
            title: `Background (${b.items.length})`,
            sub: 'SDK sessions and tooling',
            cpu: this._ctx.cpu(b.cpu),
            mem: fmt.bytes(b.rss),
        });
    }

    renderDetails() {
        syncKeyed(this.details, this._lines, this.data.items, x => x.key,
            x => new LineItem({status: !x.tooling}));
    }
});

/** A non-expandable line, used for background sessions and tooling. */
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
