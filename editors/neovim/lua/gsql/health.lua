-- `:checkhealth gsql`: is the language server installed, is the tree-sitter
-- parser loadable, and are the queries found?
local M = {}

function M.check()
  local health = vim.health
  health.start('gsql')

  if vim.fn.has('nvim-0.11') == 1 then
    health.ok('Neovim ' .. tostring(vim.version()))
  else
    health.error('Neovim 0.11 or newer is required', { 'Upgrade Neovim' })
  end

  -- The language server.
  local gsql = require('gsql')
  local server = gsql.resolve_server()
  local config = vim.lsp.config and vim.lsp.config.gsql_lsp
  local cmd = config and type(config.cmd) == 'table' and config.cmd or server.cmd
  local exe = cmd[1]
  local install_advice = {
    'Run :GsqlInstall to download the release binary into ' .. gsql.installed_path(),
    'or `cargo install --path crates/gsql-lsp`, or put gsql-lsp on $PATH',
    'or pass its path: require("gsql").setup({ cmd = { "/path/to/gsql-lsp" } })',
  }
  if vim.fn.executable(exe) == 1 then
    local result = vim.system({ exe, '--version' }, { text = true }):wait()
    local origins = {
      setup = 'from setup({ cmd })',
      path = 'found on $PATH',
      installed = 'installed by :GsqlInstall',
      folder = 'found in a standard install folder',
    }
    local origin = origins[server.origin] or 'configured command'
    if server.origin ~= 'setup' and config and type(config.cmd) == 'table' and config.cmd[1] ~= server.cmd[1] then
      origin = 'configured through vim.lsp.config'
    end
    health.ok(('language server: %s (%s, %s)'):format(vim.trim(result.stdout or ''), vim.fn.exepath(exe), origin))
  else
    health.error(('language server `%s` not found'):format(exe), install_advice)
  end
  local target, target_err = gsql.release_target()
  if target then
    health.info((':GsqlInstall downloads %s.%s from %s (%s)'):format(
      'gsql-lsp-' .. target.target, target.ext, gsql.release_base_url(), gsql.config.version or 'latest'))
    if vim.fn.executable('curl') == 0 and vim.fn.executable('wget') == 0 then
      health.warn(':GsqlInstall needs curl or wget on PATH')
    end
  else
    health.warn(':GsqlInstall is not available: ' .. target_err, { 'Build the server with `cargo install --path crates/gsql-lsp`' })
  end
  local enabled = vim.lsp.is_enabled and vim.lsp.is_enabled('gsql_lsp')
  if enabled == false then
    health.warn('the gsql_lsp configuration is not enabled', { 'Call require("gsql").setup()' })
  end

  -- The tree-sitter parser and queries.
  local ok, loaded = pcall(vim.treesitter.language.add, 'gsql')
  if ok and loaded then
    health.ok('tree-sitter parser for gsql is installed')
  else
    local advice = { 'Run :GsqlBuildParser (needs a C compiler), or :TSInstall gsql with nvim-treesitter' }
    if not gsql.compiler() then
      table.insert(advice, 'No C compiler (cc, gcc, clang or $CC) was found on PATH: install one (macOS: `xcode-select --install`)')
    end
    if vim.uv.fs_stat(gsql.grammar_dir .. '/src/parser.c') == nil then
      table.insert(advice, gsql.grammar_dir .. '/src/parser.c is missing: the plugin was not installed from the whole repository')
    end
    health.warn('tree-sitter parser for gsql is not installed; highlighting falls back to none', advice)
  end
  if gsql.compiler() then
    health.ok('C compiler for :GsqlBuildParser: ' .. gsql.compiler())
  end
  for _, name in ipairs({ 'highlights', 'folds', 'indents', 'locals' }) do
    if #vim.treesitter.query.get_files('gsql', name) == 0 then
      health.warn(('no %s query for gsql found on runtimepath'):format(name))
    end
  end

  -- The buffer you are in (run :checkhealth gsql from a .gsql buffer).
  local buf = vim.fn.bufnr('#') > 0 and vim.bo[vim.fn.bufnr('#')].filetype == 'gsql' and vim.fn.bufnr('#') or nil
  for _, candidate in ipairs(vim.api.nvim_list_bufs()) do
    if not buf and vim.bo[candidate].filetype == 'gsql' and vim.api.nvim_buf_is_loaded(candidate) then
      buf = candidate
    end
  end
  if not buf then
    health.info('open a .gsql file and run :checkhealth gsql again to check how it is served')
    return
  end
  health.start('gsql: ' .. vim.fn.fnamemodify(vim.api.nvim_buf_get_name(buf), ':~:.'))
  local clients = vim.lsp.get_clients({ bufnr = buf, name = 'gsql_lsp' })
  local client = clients[1]
  if not client then
    health.error('the language server is not attached to this buffer', {
      'Is the file type `gsql`? (:set ft?)',
      'Is the server running? Look at :LspLog',
    })
    return
  end
  health.ok(('language server attached (client %d)'):format(client.id))
  local root = client.config.root_dir
  if root then
    health.ok('project root: ' .. root .. ' (schema files are searched for under it)')
  else
    health.warn('no project root: the server only knows this file and the other .gsql files in its folder', {
      'Open Neovim in the project folder, or put a `.gsqlroot` file in the folder that holds the schema files',
      'A git root or the nearest README.md also counts as the project root',
    })
  end
  local tagfunc = vim.bo[buf].tagfunc
  if tagfunc == 'v:lua.vim.lsp.tagfunc' then
    health.ok('CTRL-] (go to definition through the tag stack) is served by the language server')
  else
    health.warn(('tagfunc is `%s`, so CTRL-] does not ask the language server'):format(tagfunc), {
      'Use `gd` / vim.lsp.buf.definition(), or set vim.bo.tagfunc = "v:lua.vim.lsp.tagfunc"',
    })
  end
end

return M
