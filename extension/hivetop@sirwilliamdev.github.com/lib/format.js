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

/** `317000` -> `317k`, `37700000` -> `37.7M`. */
export function tokens(n) {
    n = Number(n) || 0;
    if (n < 1000)
        return String(n);
    if (n < 1e6)
        return `${Math.round(n / 1000)}k`;
    return `${(n / 1e6).toFixed(1)}M`;
}

function plural(n, word) {
    return `${n} ${word}${n === 1 ? '' : 's'}`;
}

/** A change request as `PR #8`, `MR !363`, with its state when not open. */
export function change(c) {
    const [kind, mark] = c.host === 'gitlab' ? ['MR', '!'] : ['PR', '#'];
    return `${kind} ${mark}${c.number}${c.state && c.state !== 'open' ? ` ${c.state}` : ''}`;
}

/** What an agent produced, in a few words; empty when it only answered. */
export function produced(o) {
    if (!o)
        return '';
    const parts = [];
    if (o.files)
        parts.push(`${plural(o.files, 'file')} (+${o.lines_added} -${o.lines_removed})`);
    if (o.commits)
        parts.push(`${plural(o.commits, 'commit')}${o.pushes ? ', pushed' : ''}`);
    else if (o.pushes)
        parts.push('pushed');
    for (const c of o.changes ?? [])
        parts.push(change(c));
    if (o.plans)
        parts.push(o.plans === 1 ? 'plan' : `${o.plans} plans`);
    for (const [key, word] of [['docs', 'doc'], ['published', 'published page'], ['notes', 'note'],
        ['deploys', 'deploy'], ['releases', 'release'], ['restores', 'restore'], ['todos', 'to-do'],
        ['issues', 'issue'], ['messages', 'message'], ['records', 'record']]) {
        if (o[key])
            parts.push(plural(o[key], word));
    }
    if (o.memory)
        parts.push('memory');
    if (o.config)
        parts.push('config');
    return parts.join(', ');
}
