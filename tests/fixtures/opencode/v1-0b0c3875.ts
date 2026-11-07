import type { Plugin } from '@opencode-ai/plugin';

export const WorkrexStatusPlugin: Plugin = async ({ $ }) => {
  return {
    event: async ({ event }) => {
      switch (event.type) {
        case 'session.status':
          if (event.properties.status.type === 'busy') {
            await $`workrex set-window-status working`.quiet();
          }
          break;
        case 'permission.updated':
          await $`workrex set-window-status waiting`.quiet();
          break;
        case 'permission.replied':
          await $`workrex set-window-status working`.quiet();
          break;
        case 'session.idle':
          await $`workrex set-window-status done`.quiet();
          break;
      }
    },
  };
};
