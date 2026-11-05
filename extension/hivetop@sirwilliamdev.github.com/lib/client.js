import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const BUS = 'io.github.sirwilliamdev.Hivetop';
const PATH = '/io/github/sirwilliamdev/Hivetop';
const IFACE = 'io.github.sirwilliamdev.Hivetop';

export function isCancelled(e) {
    return e instanceof GLib.Error && e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED);
}

/**
 * Thin D-Bus client for hivetopd. All data comes from the daemon; the
 * extension never reads /proc itself.
 */
export class Client {
    constructor({onSummary, onAvailability}) {
        this._cancellable = new Gio.Cancellable();
        this._signalId = Gio.DBus.session.signal_subscribe(
            BUS, IFACE, 'Updated', PATH, null, Gio.DBusSignalFlags.NONE,
            (_conn, _sender, _path, _iface, _signal, params) => {
                try {
                    onSummary(JSON.parse(params.deepUnpack()[0]));
                } catch (e) {
                    console.warn(`hivetop: bad summary: ${e}`);
                }
            });
        this._watchId = Gio.bus_watch_name_on_connection(
            Gio.DBus.session, BUS, Gio.BusNameWatcherFlags.NONE,
            () => onAvailability(true),
            () => onAvailability(false));
    }

    _call(method, params, replyType) {
        return new Promise((resolve, reject) => {
            Gio.DBus.session.call(
                BUS, PATH, IFACE, method, params,
                replyType ? new GLib.VariantType(replyType) : null,
                Gio.DBusCallFlags.NONE, 5000, this._cancellable,
                (conn, res) => {
                    try {
                        resolve(conn.call_finish(res));
                    } catch (e) {
                        reject(e);
                    }
                });
        });
    }

    async snapshot(detail) {
        const reply = await this._call('GetSnapshot', new GLib.Variant('(b)', [detail]), '(s)');
        return JSON.parse(reply.deepUnpack()[0]);
    }

    async setActive(open) {
        await this._call('SetActive', new GLib.Variant('(b)', [open]), null);
    }

    async history(range, target) {
        const reply = await this._call('GetHistory', new GLib.Variant('(ss)', [range, target]), '(s)');
        return JSON.parse(reply.deepUnpack()[0]);
    }

    async kill(kind, id) {
        const reply = await this._call('Kill', new GLib.Variant('(ss)', [kind, id]), '(b)');
        return reply.deepUnpack()[0];
    }

    destroy() {
        this._cancellable.cancel();
        Gio.DBus.session.signal_unsubscribe(this._signalId);
        Gio.bus_unwatch_name(this._watchId);
    }
}
