import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {PACKAGE_VERSION} from 'resource:///org/gnome/shell/misc/config.js';

import * as fmt from './format.js';
import {Sparkline} from './sparkline.js';

const SHELL_MAJOR = parseInt(PACKAGE_VERSION, 10);
const CENTER = Clutter.ActorAlign.CENTER;

/** St.BoxLayout `vertical` became `orientation` in GNOME 48. */
export function vbox(params = {}) {
    const orientation = SHELL_MAJOR >= 48
        ? {orientation: Clutter.Orientation.VERTICAL}
        : {vertical: true};
    return new St.BoxLayout({...orientation, ...params});
}

export function ellipsized(styleClass, text = '') {
    const label = new St.Label({style_class: styleClass, text, y_align: CENTER});
    label.clutter_text.ellipsize = Pango.EllipsizeMode.END;
    return label;
}

/** A right-aligned number cell of fixed minimum width. */
export function numCell(styleClass = 'hivetop-num') {
    const label = new St.Label({x_align: Clutter.ActorAlign.END, y_align: CENTER, x_expand: true});
    const bin = new St.Bin({style_class: styleClass, child: label, y_align: CENTER});
    return [bin, label];
}

export function button(label, onClick, extraClass = '') {
    const b = new St.Button({label, style_class: `button hivetop-btn ${extraClass}`, can_focus: true});
    b.connect('clicked', () => onClick());
    return b;
}

export function iconButton(iconName, onClick, accessibleName = '') {
    const b = new St.Button({
        style_class: 'button hivetop-icon-btn',
        can_focus: true,
        accessible_name: accessibleName,
        child: new St.Icon({icon_name: iconName, icon_size: 14}),
        y_align: CENTER,
    });
    b.connect('clicked', () => onClick());
    return b;
}

/** First click arms the button for 3 s, second click confirms. */
export function confirmButton(label, onConfirm, extraClass = '') {
    const b = new St.Button({label, style_class: `button hivetop-btn ${extraClass}`, can_focus: true, y_align: CENTER});
    let armed = 0;
    const disarm = () => {
        if (armed)
            GLib.source_remove(armed);
        armed = 0;
        b.label = label;
        b.remove_style_class_name('hivetop-armed');
    };
    b.connect('clicked', () => {
        if (armed) {
            disarm();
            onConfirm();
            return;
        }
        b.label = 'Confirm?';
        b.add_style_class_name('hivetop-armed');
        armed = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 3000, () => {
            armed = 0;
            disarm();
            return GLib.SOURCE_REMOVE;
        });
    });
    b.connect('destroy', () => {
        if (armed)
            GLib.source_remove(armed);
        armed = 0;
    });
    return b;
}

const SUB2_VARIANTS = ['cmd', 'wait', 'out'];

/**
 * `[status dot] [icon] [title chip / subtitle / second subtitle] [sparkline] [cpu] [mem]`
 * The chip and the second subtitle show only when set.
 */
export class StatLine {
    constructor({spark = false, icon = false, status = false, sub2 = false} = {}) {
        this.actor = new St.BoxLayout({style_class: 'hivetop-line', x_expand: true});
        if (status) {
            this.dot = new St.Widget({style_class: 'hivetop-status', y_align: CENTER});
            this.actor.add_child(this.dot);
        }
        if (icon) {
            this.iconBin = new St.Bin({style_class: 'hivetop-app-icon', y_align: CENTER});
            this.actor.add_child(this.iconBin);
        }
        const text = vbox({x_expand: true, y_align: CENTER});
        const titleRow = new St.BoxLayout();
        this.title = ellipsized('hivetop-title');
        this.chip = new St.Label({style_class: 'hivetop-chip', y_align: CENTER, visible: false});
        titleRow.add_child(this.title);
        titleRow.add_child(this.chip);
        this.sub = ellipsized('hivetop-sub');
        text.add_child(titleRow);
        text.add_child(this.sub);
        if (sub2) {
            this.sub2 = ellipsized('hivetop-sub hivetop-sub2');
            this.sub2.visible = false;
            text.add_child(this.sub2);
        }
        this.actor.add_child(text);
        if (spark) {
            this.spark = new Sparkline();
            this.spark.y_align = CENTER;
            this.actor.add_child(this.spark);
        }
        [this.cpuBin, this.cpu] = numCell('hivetop-num hivetop-num-cpu');
        [this.memBin, this.mem] = numCell();
        this.actor.add_child(this.cpuBin);
        this.actor.add_child(this.memBin);
    }

    set({title = '', sub = '', cpu = '', mem = '', chip = ''}) {
        this.title.text = title;
        this.sub.text = sub;
        this.sub.visible = sub !== '';
        this.chip.text = chip;
        this.chip.visible = chip !== '';
        this.cpu.text = cpu;
        this.mem.text = mem;
    }

    /** `variant` is `cmd` (a command), `wait` (why an agent waits) or `out` (what it produced). */
    setSub2(text = '', variant = 'out') {
        if (!this.sub2)
            return;
        this.sub2.text = text;
        this.sub2.visible = text !== '';
        for (const v of SUB2_VARIANTS) {
            if (v === variant)
                this.sub2.add_style_class_name(`hivetop-sub2-${v}`);
            else
                this.sub2.remove_style_class_name(`hivetop-sub2-${v}`);
        }
    }

    setStatus(status) {
        if (!this.dot)
            return;
        for (const s of ['busy', 'idle', 'wait', 'orphan'])
            this.dot.remove_style_class_name(`hivetop-status-${s}`);
        this.dot.add_style_class_name(`hivetop-status-${status}`);
    }
}

/** Two toggle buttons; `onSelect(name)` runs when the selection changes. */
export class TabBar {
    constructor(names, onSelect) {
        this.item = plainItem('hivetop-tabs-item');
        const box = new St.BoxLayout({style_class: 'hivetop-tabs', x_expand: true});
        this._buttons = new Map();
        for (const [name, label] of names) {
            const b = new St.Button({
                label,
                style_class: 'hivetop-tab',
                toggle_mode: true,
                can_focus: true,
                x_expand: true,
            });
            b.connect('clicked', () => this.select(name, true));
            this._buttons.set(name, b);
            box.add_child(b);
        }
        this.item.add_child(box);
        this._onSelect = onSelect;
        this.current = names[0][0];
        this._sync();
    }

    select(name, notify = false) {
        const changed = name !== this.current;
        this.current = name;
        this._sync();
        if (changed && notify)
            this._onSelect(name);
    }

    _sync() {
        for (const [name, b] of this._buttons)
            b.checked = name === this.current;
    }
}

export function plainItem(styleClass = '') {
    return new PopupMenu.PopupBaseMenuItem({reactive: false, can_focus: false, style_class: styleClass || null});
}

export function actionItem(buttons) {
    const item = plainItem('hivetop-actions');
    const box = new St.BoxLayout({style_class: 'hivetop-action-box', x_expand: true});
    for (const b of buttons)
        box.add_child(b);
    item.add_child(box);
    return item;
}

/**
 * Keeps `section` showing one item per element of `items`, in order, reusing
 * items by key. GNOME Shell keeps memory for every menu item destroyed, so
 * rows are updated in place instead of rebuilt on each refresh. A submenu row
 * moves together with its submenu.
 */
export function syncKeyed(section, cache, items, keyOf, create) {
    const seen = new Set();
    const ordered = [];
    for (const it of items) {
        const key = keyOf(it);
        seen.add(key);
        let item = cache.get(key);
        if (!item) {
            item = create(it);
            cache.set(key, item);
            section.addMenuItem(item);
        }
        item.update(it);
        ordered.push(item);
    }
    for (const [key, item] of cache) {
        if (!seen.has(key)) {
            item.destroy();
            cache.delete(key);
        }
    }
    const box = section.box;
    const actors = [];
    for (const item of ordered) {
        actors.push(item);
        if (item.menu?.actor?.get_parent() === box)
            actors.push(item.menu.actor);
    }
    const current = box.get_children().filter(a => actors.includes(a));
    if (current.length === actors.length && current.every((a, i) => a === actors[i]))
        return;
    let prev = null;
    for (const actor of actors) {
        if (prev)
            box.set_child_above_sibling(actor, prev);
        else
            box.set_child_below_sibling(actor, null);
        prev = actor;
    }
}

export const ProcessItem = GObject.registerClass(
class HivetopProcessItem extends PopupMenu.PopupBaseMenuItem {
    _init(ctx, {killable = true} = {}) {
        super._init({reactive: false, can_focus: false, style_class: 'hivetop-proc'});
        this._ctx = ctx;
        const box = new St.BoxLayout({style_class: 'hivetop-line', x_expand: true});
        this._pid = new St.Label({style_class: 'hivetop-pid', y_align: CENTER});
        this._cmd = ellipsized('hivetop-cmd');
        this._cmd.x_expand = true;
        [this._cpuBin, this._cpu] = numCell('hivetop-num hivetop-num-cpu');
        [this._memBin, this._mem] = numCell();
        for (const a of [this._pid, this._cmd, this._cpuBin, this._memBin])
            box.add_child(a);
        box.add_child(iconButton('edit-copy-symbolic',
            () => ctx.copy(`pid: ${this._p.pid}\ncommand: ${this._p.cmd}`), 'Copy'));
        if (killable) {
            box.add_child(confirmButton('Kill',
                () => ctx.kill('pid', `${this._p.pid}:${this._p.start}`), 'hivetop-btn-small'));
        }
        this.add_child(box);
    }

    update(p) {
        this._p = p;
        this._pid.text = String(p.pid);
        this._cmd.text = p.cmd;
        if (p.infra)
            this._cmd.add_style_class_name('hivetop-infra');
        else
            this._cmd.remove_style_class_name('hivetop-infra');
        this._cpu.text = this._ctx.cpu(p.cpu);
        this._mem.text = fmt.bytes(p.rss);
    }
});

export const ContainerItem = GObject.registerClass(
class HivetopContainerItem extends PopupMenu.PopupBaseMenuItem {
    _init(ctx, {stoppable = false} = {}) {
        super._init({reactive: false, can_focus: false, style_class: 'hivetop-proc'});
        this._ctx = ctx;
        const box = new St.BoxLayout({style_class: 'hivetop-line', x_expand: true});
        const icon = new St.Icon({icon_name: 'application-x-firmware-symbolic', icon_size: 14, y_align: CENTER});
        this._name = ellipsized('hivetop-cmd');
        this._name.x_expand = true;
        [this._cpuBin, this._cpu] = numCell('hivetop-num hivetop-num-cpu');
        [this._memBin, this._mem] = numCell();
        for (const a of [icon, this._name, this._cpuBin, this._memBin])
            box.add_child(a);
        box.add_child(iconButton('edit-copy-symbolic', () => {
            const c = this._c;
            ctx.copy(`container: ${c.name}\nid: ${c.id}\nimage: ${c.image}\nproject: ${c.project}\nworking dir: ${c.working_dir}`);
        }, 'Copy'));
        if (stoppable)
            box.add_child(confirmButton('Stop', () => ctx.kill('container', this._c.id), 'hivetop-btn-small'));
        this.add_child(box);
    }

    update(c) {
        this._c = c;
        this._name.text = `${c.name}${c.oneoff ? '  (run)' : ''}`;
        this._cpu.text = this._ctx.cpu(c.cpu);
        this._mem.text = fmt.bytes(c.mem);
    }
});

/**
 * Scrolls `scrollView` so the span from `top` to `bottom` (actors inside it) is
 * visible, preferring the top when the span is taller than the view.
 */
export function ensureVisible(scrollView, top, bottom = top) {
    const adjustment = scrollView.vadjustment ?? scrollView.vscroll?.adjustment;
    const content = scrollView.get_child?.() ?? scrollView.child;
    if (!adjustment || !content)
        return;
    const offset = actor => {
        let y = 0;
        for (let a = actor; a; a = a.get_parent()) {
            if (a === content)
                return y;
            y += a.get_allocation_box().y1;
        }
        return null;
    };
    const y1 = offset(top);
    const y2 = offset(bottom);
    if (y1 === null || y2 === null)
        return;
    const end = y2 + bottom.height;
    const [value, , , , , pageSize] = adjustment.get_values();
    if (y1 < value)
        adjustment.value = y1;
    else if (end > value + pageSize)
        adjustment.value = Math.min(y1, end - pageSize);
}

/** Runs `fn` once after `ms`, unless `owner` is destroyed first. */
export function later(owner, ms, fn) {
    const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
        owner.disconnect(destroyId);
        fn();
        return GLib.SOURCE_REMOVE;
    });
    const destroyId = owner.connect('destroy', () => GLib.source_remove(id));
}

/**
 * Clickable section title with a chevron. It toggles instead of activating,
 * so clicking it never closes the menu.
 */
export const SectionHeader = GObject.registerClass(
class HivetopSectionHeader extends PopupMenu.PopupBaseMenuItem {
    /** A `nested` header heads a group of rows inside a section and looks like a row. */
    _init(onToggle, {nested = false} = {}) {
        super._init({style_class: nested ? 'hivetop-section-header hivetop-subsection-header' : 'hivetop-section-header'});
        this._onToggle = onToggle;
        this.expanded = false;
        this._arrow = new St.Icon({icon_name: 'pan-end-symbolic', icon_size: 16, y_align: CENTER});
        this.add_child(this._arrow);
        this.line = new StatLine();
        if (!nested)
            this.line.title.add_style_class_name('hivetop-section-title');
        this.add_child(this.line.actor);
    }

    activate(_event) {
        this.setExpanded(!this.expanded);
        this._onToggle(this.expanded);
    }

    setExpanded(expanded) {
        this.expanded = expanded;
        this._arrow.icon_name = expanded ? 'pan-down-symbolic' : 'pan-end-symbolic';
    }
});
