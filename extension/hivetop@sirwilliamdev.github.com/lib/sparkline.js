import GObject from 'gi://GObject';
import St from 'gi://St';

const CPU_COLOR = [0.38, 0.62, 0.95];
const MEM_COLOR = [0.96, 0.56, 0.20];

/**
 * Two-line chart (CPU and memory) over a time window. Points are
 * `[unix seconds, cpu, mem]`. Size comes from the style class.
 */
export const Sparkline = GObject.registerClass(
class HivetopSparkline extends St.DrawingArea {
    _init(styleClass = 'hivetop-spark') {
        super._init({style_class: styleClass});
        this._points = [];
        this._span = 600;
        this._cpuMax = null;
        this._memMax = null;
        this.connect('repaint', () => this._draw());
    }

    setData(points, {span = 600, cpuMax = null, memMax = null, end = null} = {}) {
        this._points = points ?? [];
        this._span = span;
        this._cpuMax = cpuMax;
        this._memMax = memMax;
        this._end = end;
        this.queue_repaint();
    }

    _draw() {
        const cr = this.get_context();
        const [w, h] = this.get_surface_size();
        const pts = this._points;
        if (pts.length >= 2) {
            const t1 = this._end ?? pts[pts.length - 1][0];
            const span = this._end ? this._span : Math.min(this._span, Math.max(60, t1 - pts[0][0]));
            const t0 = t1 - span;
            const series = [[1, CPU_COLOR, this._cpuMax], [2, MEM_COLOR, this._memMax]];
            for (const [idx, color, fixedMax] of series) {
                const max = fixedMax ?? Math.max(...pts.map(p => p[idx]), 1e-9);
                cr.setSourceRGBA(color[0], color[1], color[2], 0.95);
                cr.setLineWidth(1.5);
                let started = false;
                for (const p of pts) {
                    if (p[0] < t0)
                        continue;
                    const x = ((p[0] - t0) / span) * (w - 1);
                    const y = h - 1 - (Math.min(p[idx], max) / max) * (h - 2);
                    if (started) {
                        cr.lineTo(x, y);
                    } else {
                        cr.moveTo(x, y);
                        started = true;
                    }
                }
                cr.stroke();
            }
        }
        cr.$dispose();
    }
});
