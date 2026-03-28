// Console implementation using Deno.core.print
// __log_prefix__ is injected by the runtime before this file loads
const console = {
    log: function(...args) {
        const output = args.map(arg => {
            if (typeof arg === 'object') {
                return JSON.stringify(arg);
            }
            return String(arg);
        }).join(' ');
        Deno.core.print('[' + __log_prefix__ + '] ' + output + '\n');
    },
    error: function(...args) {
        const output = args.map(arg => {
            if (typeof arg === 'object') {
                return JSON.stringify(arg);
            }
            return String(arg);
        }).join(' ');
        Deno.core.print('[' + __log_prefix__ + '] ERROR: ' + output + '\n');
    }
};
