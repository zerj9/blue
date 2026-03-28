// Basic console implementation for hook runtime
const console = {
    log: function(...args) {
        const output = args.map(arg => {
            if (typeof arg === 'object') {
                return JSON.stringify(arg);
            }
            return String(arg);
        }).join(' ');
        
        // Store output in global object for retrieval
        if (typeof __hook_output__ === 'undefined') {
            __hook_output__ = '';
        }
        __hook_output__ += output + '\n';
    },
    error: function(...args) {
        const output = args.map(arg => {
            if (typeof arg === 'object') {
                return JSON.stringify(arg);
            }
            return String(arg);
        }).join(' ');
        
        if (typeof __hook_output__ === 'undefined') {
            __hook_output__ = '';
        }
        __hook_output__ += 'ERROR: ' + output + '\n';
    }
};