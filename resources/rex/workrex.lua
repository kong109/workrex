-- Opens the workrex dashboard in a floating layer over the current tab.
-- Copy this file to ~/.config/rex/workrex.lua and add require("workrex")
-- to ~/.config/rex/init.lua.

local TERMINAL = "com.superlogical.terminal"
local LABEL = "workrex-dashboard"
local WORKREX = os.getenv("WORKREX_BIN") or "workrex"

local function blocks(window)
  local found = {}
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.blocks or {}) do
      table.insert(found, block)
    end
  end
  return found
end

-- The app keeps its own current tab, which can differ from the session's
-- active window, so the layer goes into the tab of the pane the key was
-- pressed in.
local function target_window(view, block_id)
  for _, window in ipairs(view.windows or {}) do
    for _, block in ipairs(blocks(window)) do
      if block.block_id == block_id then
        return window
      end
    end
  end
  for _, window in ipairs(view.windows or {}) do
    if window.window_id == view.active_window_id then
      return window
    end
  end
end

local function find_dashboard(window)
  for _, block in ipairs(blocks(window)) do
    if block.label == LABEL then
      return block.block_id
    end
  end
end

local function working_dir(block_id)
  local process = rex.block.call(TERMINAL, "process", { block_id = block_id })
  if process == nil then
    return nil
  end
  for _, entry in ipairs({ process.foreground, process.child }) do
    if entry ~= nil and entry.cwd ~= nil then
      return entry.cwd
    end
  end
end

rex.action{
  name = "workrex_dashboard",
  title = "workrex: Open Dashboard",
  description = "Show every workrex agent in a floating layer.",
  category = "workrex",
  keywords = "workrex workmux agents dashboard",
  args = { args = "array?" },
  run = function(ctx, args)
    local view, err = rex.session.view{}
    if view == nil then
      error(err, 0)
    end
    local window = target_window(view, ctx.block_id)
    if window == nil then
      error("no window to open the dashboard in", 0)
    end
    local existing = find_dashboard(window)
    if existing ~= nil then
      rex.session.focus_block{ block_id = existing }
      return { block_id = existing }
    end

    -- REX_CLIENT makes the dashboard switch sessions through the app that
    -- opened it. The origin block stands in for the dashboard's own block
    -- when the dashboard asks which pane is active.
    local command = { "env" }
    if ctx.client_id ~= nil then
      table.insert(command, "REX_CLIENT=" .. ctx.client_id)
    end
    if ctx.block_id ~= nil then
      table.insert(command, "WORKREX_REX_ORIGIN_BLOCK=" .. ctx.block_id)
    end
    table.insert(command, WORKREX)
    table.insert(command, "dashboard")
    for _, arg in ipairs(args.args or {}) do
      table.insert(command, arg)
    end

    local layer, layer_err = rex.session.new_layer{
      window_id = window.window_id,
      bounds = { 0.05, 0.05, 0.9, 0.9 },
      focus = true,
      layout = {
        block = {
          flavor = TERMINAL .. ".shell",
          label = LABEL,
          options = {
            command = command,
            cwd = ctx.block_id and working_dir(ctx.block_id),
          },
        },
      },
    }
    if layer == nil then
      error(layer_err, 0)
    end
    return { layer_id = layer.layer_id, block_id = layer.block_ids[1] }
  end,
}

-- Replaces Rex's Split Pane Down shortcut, which stays on shift+cmd+down.
rex.bind("shift+cmd+d", "workrex_dashboard")
