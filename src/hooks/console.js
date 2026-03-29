// Console implementation using Deno.core.print
// __log_prefix__ is injected by the runtime before this file loads
function __formatArgs(args) {
    return args.map(function(arg) {
        if (typeof arg === 'object') {
            return JSON.stringify(arg);
        }
        return String(arg);
    }).join(' ');
}

const console = {
    log: function(...args) {
        Deno.core.print('[' + __log_prefix__ + '] ' + __formatArgs(args) + '\n');
    },
    error: function(...args) {
        Deno.core.print('[' + __log_prefix__ + '] ERROR: ' + __formatArgs(args) + '\n');
    }
};
