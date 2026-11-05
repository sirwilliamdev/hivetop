import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import St from 'gi://St';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {isCancelled} from './client.js';
import * as fmt from './format.js';
import {LineItem, appName} from './rows.js';
import {Sparkline} from './sparkline.js';
import {plainItem, syncKeyed, vbox} from './widgets.js';

const RANGES = ['1h', '24h', '7d', '30d'];
const KIND_LABEL = {session: 'Claude session', app: 'App', container: 'Container'};

export const HistoryRow = GObject.registerClass(
class HivetopHistoryRow extends PopupMenu.PopupSubMenuMenuItem {
    _init(ctx) {
        super._init('History', false);
        this._ctx = ctx;
        this._range = '24h';
        this._buttons = new Map();

        const ranges = new St.BoxLayout({style_class: 'hivetop-action-box'});
        for (const r of RANGES) {
            const b = new St.Button({label: r, style_class: 'button hivetop-btn', toggle_mode: true, can_focus: true});
            b.connect('clicked', () => this._select(r));
            this._buttons.set(r, b);
            ranges.add_child(b);
        }
        const rangeItem = plainItem('hivetop-actions');
        rangeItem.add_child(ranges);
        this.menu.addMenuItem(rangeItem);

        const chartBox = vbox({x_expand: true});
        this._chart = new Sparkline('hivetop-chart');
        this._legend = new St.Label({style_class: 'hivetop-sub', y_align: Clutter.ActorAlign.CENTER});
        chartBox.add_child(this._chart);
        chartBox.add_child(this._legend);
        const chartItem = plainItem('hivetop-chart-item');
        chartItem.add_child(chartBox);
        this.menu.addMenuItem(chartItem);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem('Top by average RAM  (avg / peak, avg CPU)'));
        this._ranking = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._ranking);
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._load();
        });
    }

    _select(range) {
        this._range = range;
        this._load();
    }

    async _load() {
        for (const [r, b] of this._buttons)
            b.checked = r === this._range;
        let h;
        try {
            h = await this._ctx.client.history(this._range, '');
        } catch (e) {
            if (!isCancelled(e))
                this._legend.text = 'History unavailable: is hivetopd running?';
            return;
        }
        if (h.error) {
            this._legend.text = `History unavailable: ${h.error}`;
            return;
        }
        const points = h.points.map(p => [p[0], p[1], p[3]]);
        this._chart.setData(points, {
            span: h.to - h.from,
            end: h.to,
            cpuMax: 100,
            memMax: this._ctx.memTotal() || null,
        });
        if (points.length === 0) {
            this._legend.text = 'No data yet: history is written once a minute.';
        } else {
            const cpuAvg = h.points.reduce((n, p) => n + p[1], 0) / h.points.length;
            const cpuPeak = Math.max(...h.points.map(p => p[2]));
            const memAvg = h.points.reduce((n, p) => n + p[3], 0) / h.points.length;
            const memPeak = Math.max(...h.points.map(p => p[4]));
            this._legend.text = `CPU (blue) avg ${fmt.pct(cpuAvg)}, peak ${fmt.pct(cpuPeak)}   ` +
                `RAM (orange) avg ${fmt.bytes(memAvg)}, peak ${fmt.bytes(memPeak)}`;
        }
        this._rankItems ??= new Map();
        syncKeyed(this._ranking, this._rankItems, h.ranking, r => `${r.kind}:${r.key}`, () => {
            const item = new LineItem();
            item.line.memBin.add_style_class_name('hivetop-num-wide');
            return item;
        });
        for (const r of h.ranking) {
            this._rankItems.get(`${r.kind}:${r.key}`).update({
                title: r.kind === 'app' ? appName(r.label) : r.label,
                sub: `${KIND_LABEL[r.kind] ?? r.kind}, active ${fmt.ago(r.minutes * 60)}`,
                mem: `${fmt.bytes(r.mem_avg)} / ${fmt.bytes(r.mem_max)}`,
                cpu: this._ctx.cpu(r.cpu_avg),
            });
        }
    }
});
