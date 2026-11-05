export function bytes(n) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let v = Number(n) || 0;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
        v /= 1024;
        i++;
    }
    return i >= 3 ? `${v.toFixed(1)} ${units[i]}` : `${Math.round(v)} ${units[i]}`;
}

export function pct(v) {
    return `${Math.round(Number(v) || 0)}%`;
}

/** A share of the whole machine: one decimal below 10%. */
export function share(v) {
    v = Number(v) || 0;
    return v > 0 && v < 9.95 ? `${v.toFixed(1)}%` : `${Math.round(v)}%`;
}

export function ago(secs) {
    secs = Math.max(0, Math.round(secs));
    if (secs < 60)
        return `${secs}s`;
    if (secs < 3600)
        return `${Math.floor(secs / 60)}m`;
    if (secs < 86400)
        return `${Math.floor(secs / 3600)}h${String(Math.floor(secs % 3600 / 60)).padStart(2, '0')}m`;
    return `${Math.floor(secs / 86400)}d${Math.floor(secs % 86400 / 3600)}h`;
}

export function homeShort(path, home) {
    return home && path.startsWith(home) ? `~${path.slice(home.length)}` : path;
}
