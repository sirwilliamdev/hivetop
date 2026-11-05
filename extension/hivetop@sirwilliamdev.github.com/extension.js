import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {Indicator} from './lib/indicator.js';

export default class HivetopExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._addIndicator();
        this._positionIds = ['changed::panel-position', 'changed::panel-index'].map(signal =>
            this._settings.connect(signal, () => {
                this._removeIndicator();
                this._addIndicator();
            }));
    }

    disable() {
        for (const id of this._positionIds ?? [])
            this._settings.disconnect(id);
        this._positionIds = null;
        this._removeIndicator();
        this._settings = null;
    }

    _addIndicator() {
        this._indicator = new Indicator(this, this._settings);
        Main.panel.addToStatusArea(
            this.uuid,
            this._indicator,
            this._settings.get_int('panel-index'),
            this._settings.get_string('panel-position'));
    }

    _removeIndicator() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
