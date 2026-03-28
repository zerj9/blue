// Test hook script
console.log("Hook executed successfully!");
console.log("Resource type:", context.resources?.server?.type);
console.log("Current phase:", context.current_phase);

// Return some output
__hook_output__ = "Hook completed successfully\n";