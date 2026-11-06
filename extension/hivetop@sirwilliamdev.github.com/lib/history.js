import Clutter from 'gi://Clutter';
import St from 'gi://St';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {isCancelled} from './client.js';
import * as fmt from './format.js';
import {LineItem, appName} from './rows.js';
import {Sparkline} from './sparkline.js';
import {SectionHeader, ellipsized, later, plainItem, syncKeyed, vbox} from './widgets.js';

const RANGES = ['1h', '24h', '7d', '30d'];
const KIND_LABEL = {session: 'Claude session', app: 'App', container: 'Container'};

function avg(points, idx) {
    return points.reduce((n, p) => n + p[idx], 0) / points.length;
}

function peak(points, idx) {
    return points.reduce((n, p) => Math.max(n, p[idx]), 0);
}

/**
 * Collapsible history section inside the menu's scroll view, so the menu
 * scrolls it like every other section. Data is fetched each time the section
 * expands or the range changes; rows are reused across loads.
 */
export class HistorySection {
    constructor(menu, ctx) {
        this._ctx = ctx;
        this._range = '24h';
        this._buttons = new Map();
        this._rankItems = new Map();
        this._seq = 0;

        this.header = new SectionHeader(expanded => {
            this.section.actor.visible = expanded;
            if (expanded) {
                this._reveal();
                this._load();
            }
        });
        this.header.line.set({title: 'History', sub: 'CPU and RAM over time, top consumers'});
        this.section = new PopupMenu.PopupMenuSection();
        menu.addMenuItem(this.header);
        menu.addMenuItem(this.section);
        this.section.actor.visible = false;

        const ranges = new St.BoxLayout({style_class: 'hivetop-action-box'});
        for (const r of RANGES) {
            const b = new St.Button({label: r, style_class: 'button hivetop-btn', toggle_mode: true, can_focus: true});
            b.connect('clicked', () => this._select(r));
            this._buttons.set(r, b);
            ranges.add_child(b);
        }
        const rangeItem = plainItem('hivetop-actions');
        rangeItem.add_child(ranges);
        this.section.addMenuItem(rangeItem);

        const chartBox = vbox({x_expand: true});
        this._chart = new Sparkline('hivetop-chart');
        this._legend = ellipsized('hivetop-sub hivetop-legend');
        this._legend.y_align = Clutter.ActorAlign.CENTER;
        chartBox.add_child(this._chart);
        chartBox.add_child(this._legend);
        const chartItem = plainItem('hivetop-chart-item');
        chartItem.add_child(chartBox);
        this.section.addMenuItem(chartItem);

        this.section.addMenuItem(new PopupMenu.PopupSeparatorMenuItem('Top by average RAM  (CPU avg / peak, RAM avg / peak)'));
        this._ranking = new PopupMenu.PopupMenuSection();
        this.section.addMenuItem(this._ranking);
        this._syncButtons();
    }

    collapse() {
        this.header.setExpanded(false);
        this.section.actor.visible = false;
    }

    _reveal() {
        later(this.header, 50, () => this._ctx.reveal(this.header, this.section.actor));
    }

    _select(range) {
        this._range = range;
        this._load();
    }

    _syncButtons() {
        for (const [r, b] of this._buttons)
            b.checked = r === this._range;
    }

    async _load() {
        this._syncButtons();
        const seq = ++this._seq;
        this._legend.text = `Loading ${this._range}...`;
        let h;
        try {
            h = await this._ctx.client.history(this._range, '');
        } catch (e) {
            if (seq === this._seq && !isCancelled(e))
                this._legend.text = 'History unavailable: is hivetopd running?';
            return;
        }
        if (seq !== this._seq || !this.header.expanded)
            return;
        if (h.error) {
            this._legend.text = `History unavailable: ${h.error}`;
            return;
        }
        this._render(h);
        this._reveal();
    }

    _render(h) {
        const pts = h.points;
        this._chart.setData(pts.map(p => [p[0], p[1], p[3]]), {
            span: h.to - h.from,
            end: h.to,
            cpuMax: 100,
            memMax: this._ctx.memTotal() || null,
        });
        if (pts.length === 0) {
            this._legend.text = 'No data yet: history is written once a minute.';
            this.header.line.set({title: 'History', sub: `last ${this._range}: no data yet`});
        } else {
            this._legend.text = `CPU (blue) avg ${fmt.pct(avg(pts, 1))}, peak ${fmt.pct(peak(pts, 2))}   ` +
                `RAM (orange) avg ${fmt.bytes(avg(pts, 3))}, peak ${fmt.bytes(peak(pts, 4))}`;
            this.header.line.set({
                title: 'History',
                sub: `last ${this._range}: average CPU and RAM`,
                cpu: fmt.pct(avg(pts, 1)),
                mem: fmt.bytes(avg(pts, 3)),
            });
        }
        syncKeyed(this._ranking, this._rankItems, h.ranking, r => `${r.kind}:${r.key}`, () => {
            const item = new LineItem();
            item.line.cpuBin.add_style_class_name('hivetop-num-pair');
            item.line.memBin.add_style_class_name('hivetop-num-wide');
            return item;
        });
        for (const r of h.ranking) {
            this._rankItems.get(`${r.kind}:${r.key}`).update({
                title: r.kind === 'app' ? appName(r.label) : r.label,
                sub: `${KIND_LABEL[r.kind] ?? r.kind}, active ${fmt.ago(r.minutes * 60)}`,
                cpu: `${this._ctx.cpu(r.cpu_avg)} / ${this._ctx.cpu(r.cpu_max)}`,
                mem: `${fmt.bytes(r.mem_avg)} / ${fmt.bytes(r.mem_max)}`,
            });
        }
    }
}
