-- Tests the repository root as a plugin, the way lazy.nvim and vim.pack install
-- it (the whole repository, the plugin directory added to 'runtimepath' and its
-- plugin/ files sourced): the root-level lua/ and plugin/ shims must make
-- `require('gsql')`, filetype detection, ftplugin, queries, the LSP config and
-- the parser build work from editors/neovim.
--
--   nvim --headless --clean -u NONE -l editors/neovim/test/layout.lua
--
-- With LAZY_NVIM=<path to lazy.nvim> it also loads the copy through the real
-- lazy.nvim, in a child Neovim with private XDG directories.

local repo = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h:h:h')

local failures = 0
local function check(condition, message)
  if condition then
    print('ok   ' .. message)
  else
    failures = failures + 1
    print('FAIL ' .. message)
  end
end

local function finish()
  print(failures == 0 and 'all checks passed' or (failures .. ' check(s) failed'))
  vim.cmd(failures == 0 and 'qall!' or 'cquit!')
end

local function sh(cmd, opts)
  local result = vim.system({ 'sh', '-c', cmd }, vim.tbl_extend('force', { text = true }, opts or {})):wait()
  return result
end

-- A copy of the repository without build output, like a fresh clone.
local tmp = vim.fn.tempname()
local root = tmp .. '/gsql-lsp'
vim.fn.mkdir(root, 'p')
local copied = sh(('cd %s && cp -R lua plugin editors tree-sitter-gsql %s/ && rm -rf %s/editors/neovim/parser'):format(
  vim.fn.shellescape(repo), vim.fn.shellescape(root), vim.fn.shellescape(root)))
check(copied.code == 0, 'copied the repository layout: ' .. (copied.stderr or ''))

local plugin = root .. '/editors/neovim'
local function count(list, value)
  local n = 0
  for _, item in ipairs(list) do
    if vim.fs.normalize(item) == vim.fs.normalize(value) then
      n = n + 1
    end
  end
  return n
end

-- What the package manager does: the clone's root on 'runtimepath', then its plugin/ files.
vim.opt.rtp:prepend(root)
check(vim.fn.exists(':GsqlInstall') == 0, 'no commands before the plugin is sourced')
vim.cmd('source ' .. root .. '/plugin/gsql.lua')

local gsql = require('gsql')
check(gsql.plugin_dir == plugin, 'require("gsql") is the module in editors/neovim: ' .. gsql.plugin_dir)
check(gsql.grammar_dir == root .. '/tree-sitter-gsql', 'the grammar is found relative to the repository root')
check(count(vim.opt.rtp:get(), plugin) == 1, 'editors/neovim is on runtimepath exactly once')
vim.cmd('source ' .. root .. '/plugin/gsql.lua')
package.loaded.gsql = nil
require('gsql')
check(count(vim.opt.rtp:get(), plugin) == 1, '... also after sourcing and requiring again')
check(vim.fn.exists(':GsqlInstall') == 2 and vim.fn.exists(':GsqlUpdate') == 2 and vim.fn.exists(':GsqlBuildParser') == 2, 'the user commands exist')
check(vim.filetype.match({ filename = 'a.gsql' }) == 'gsql', 'ftdetect: *.gsql is detected')
check(type(require('gsql.health').check) == 'function', 'require("gsql.health") resolves')
check(#vim.treesitter.query.get_files('gsql', 'highlights') > 0, 'queries/gsql/highlights.scm is found')
check(#vim.api.nvim_get_runtime_file('lsp/gsql_lsp.lua', true) == 1, 'lsp/gsql_lsp.lua is found')

-- The parser is built from tree-sitter-gsql/src/parser.c of the copy.
local built
gsql.build_parser(function(ok)
  built = ok
end)
vim.wait(60000, function()
  return built ~= nil
end)
check(built and vim.uv.fs_stat(plugin .. '/parser/gsql.so') ~= nil, 'the parser builds into editors/neovim/parser')
check(select(1, pcall(vim.treesitter.language.add, 'gsql')), 'the parser loads')

-- ftplugin and highlighting through the runtimepath entry.
vim.cmd('filetype plugin on')
vim.fn.writefile({ 'CREATE QUERY q() FOR GRAPH g {', '  PRINT 1;', '}' }, tmp .. '/q.gsql')
vim.cmd('edit ' .. tmp .. '/q.gsql')
check(vim.bo.filetype == 'gsql', 'filetype gsql')
check(vim.bo.commentstring == '// %s', 'ftplugin ran')
check(vim.treesitter.highlighter.active[vim.api.nvim_get_current_buf()] ~= nil, 'tree-sitter highlighting started')

gsql.setup({ cmd = { 'gsql-lsp' } })
check(vim.lsp.is_enabled('gsql_lsp'), 'setup() enables gsql_lsp')
check(vim.lsp.config.gsql_lsp.filetypes[1] == 'gsql', 'the lsp config of editors/neovim is used')

-- The real lazy.nvim, when given.
local lazy = vim.env.LAZY_NVIM
if lazy and lazy ~= '' then
  local home = tmp .. '/lazyhome'
  vim.fn.mkdir(home, 'p')
  local init = tmp .. '/lazy-init.lua'
  vim.fn.writefile({
    'vim.opt.rtp:prepend(' .. vim.inspect(lazy) .. ')',
    "require('lazy').setup({",
    '  { dir = ' .. vim.inspect(root) .. ', name = "gsql-lsp", opts = { cmd = { "gsql-lsp" } } },',
    '}, { install = { missing = false }, checker = { enabled = false }, change_detection = { enabled = false },',
    '     performance = { rtp = { reset = true } }, lockfile = ' .. vim.inspect(tmp .. '/lazy-lock.json') .. ' })',
    'local out = {}',
    "local function add(s) table.insert(out, s) end",
    "add('module ' .. tostring(require('gsql').plugin_dir))",
    "add('command ' .. vim.fn.exists(':GsqlInstall'))",
    "add('ft ' .. tostring(vim.filetype.match({ filename = 'a.gsql' })))",
    "add('queries ' .. #vim.treesitter.query.get_files('gsql', 'highlights'))",
    "add('lsp ' .. tostring(vim.lsp.is_enabled('gsql_lsp')))",
    "io.stdout:write(table.concat(out, '\\n') .. '\\n')",
  }, init)
  local result = vim.system({ 'nvim', '--headless', '-u', init, '+qall!' }, {
    text = true,
    env = {
      XDG_CONFIG_HOME = home .. '/config',
      XDG_DATA_HOME = home .. '/data',
      XDG_STATE_HOME = home .. '/state',
      XDG_CACHE_HOME = home .. '/cache',
    },
  }):wait()
  local output = (result.stdout or '') .. (result.stderr or '')
  check(output:find('module ' .. plugin, 1, true) ~= nil, 'lazy.nvim: require("gsql") from a plain spec' .. (output:find('module', 1, true) and '' or ': ' .. output))
  check(output:find('command 2', 1, true) ~= nil, 'lazy.nvim: :GsqlInstall exists')
  check(output:find('ft gsql', 1, true) ~= nil, 'lazy.nvim: *.gsql is detected')
  check(output:find('queries 1', 1, true) ~= nil, 'lazy.nvim: queries are found')
  check(output:find('lsp true', 1, true) ~= nil, 'lazy.nvim: opts = {} calls setup() (main module detected)')
else
  print('skip lazy.nvim (set LAZY_NVIM=<path to lazy.nvim> to include it)')
end

vim.fn.delete(tmp, 'rf')
finish()
