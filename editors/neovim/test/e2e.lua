-- End-to-end test of the Neovim integration: tree-sitter highlighting,
-- folding and the gsql-lsp language server through Neovim's own client.
--
--   GSQL_LSP_BIN=target/debug/gsql-lsp nvim --headless --clean -u NONE -l editors/neovim/test/e2e.lua

local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.opt.rtp:prepend(plugin_dir)
vim.cmd('runtime! ftdetect/gsql.lua')
vim.cmd('runtime! plugin/gsql.lua')

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

-- Build the parser into the plugin directory.
local built
require('gsql').build_parser(function(ok)
  built = ok
end)
vim.wait(60000, function()
  return built ~= nil
end)
check(built, 'parser builds with :GsqlBuildParser')

-- A small workspace: the schema lives in its own file.
local root = vim.fn.tempname()
vim.fn.mkdir(root, 'p')
vim.fn.writefile({
  'CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)',
  'CREATE UNDIRECTED EDGE Friendship (FROM Person, TO Person, since DATETIME)',
  'CREATE GRAPH Social (Person, Friendship)',
}, root .. '/schema.gsql')
vim.fn.writefile({
  'CREATE QUERY friends(VERTEX<Person> p) FOR GRAPH Social {',
  '  SumAccum<INT> @@count;',
  '  Start = {p};',
  '  Result = SELECT t FROM Start:s -(Friendship:e)- Person:t',
  '           WHERE t.agee > 18',
  '           ACCUM @@count += 1;',
  '      PRINT Result, @@count;',
  '}',
}, root .. '/query.gsql')
vim.fn.mkdir(root .. '/.git', 'p')

local bin = vim.env.GSQL_LSP_BIN or 'gsql-lsp'
require('gsql').setup({ cmd = { vim.fn.fnamemodify(bin, ':p') } })

vim.cmd('edit ' .. root .. '/query.gsql')
local buf = vim.api.nvim_get_current_buf()
check(vim.bo[buf].filetype == 'gsql', 'filetype is detected')
check(vim.treesitter.highlighter.active[buf] ~= nil, 'tree-sitter highlighting is active')

-- Headless Neovim never redraws, so parse explicitly.
vim.treesitter.get_parser(buf):parse()
local captures = vim.treesitter.get_captures_at_pos(buf, 1, 2)
check(captures[#captures] and captures[#captures].capture == 'type.builtin', 'SumAccum is highlighted as @type.builtin')

-- Wait for the client to attach and publish diagnostics.
vim.wait(10000, function()
  return #vim.lsp.get_clients({ bufnr = buf, name = 'gsql_lsp' }) > 0
end)
local client = vim.lsp.get_clients({ bufnr = buf, name = 'gsql_lsp' })[1]
check(client ~= nil, 'gsql_lsp attaches to the buffer')
if not client then
  return finish()
end

vim.wait(10000, function()
  return #vim.diagnostic.get(buf) > 0
end)
local diagnostics = vim.diagnostic.get(buf)
check(
  #diagnostics == 1 and vim.startswith(diagnostics[1].message, '`Person` has no attribute `agee`'),
  'diagnostics use the schema from another file: ' .. vim.inspect(vim.tbl_map(function(d)
    return d.message
  end, diagnostics))
)

local function request(method, params)
  local responses = vim.lsp.buf_request_sync(buf, method, params, 5000) or {}
  for _, response in pairs(responses) do
    return response.result
  end
end

local position = function(line, character)
  return { textDocument = vim.lsp.util.make_text_document_params(buf), position = { line = line, character = character } }
end

local hover = request('textDocument/hover', position(5, 18))
check(hover and hover.contents.value:find('SumAccum<INT> @@count', 1, true), 'hover shows the accumulator declaration')

local completion = request('textDocument/completion', position(4, 19))
local labels = {}
for _, item in ipairs(completion and completion.items or {}) do
  labels[item.label] = true
end
check(labels.age and labels.name, 'completion offers vertex attributes after `t.`')

local definition = request('textDocument/definition', position(3, 52))
check(definition and definition[1] and definition[1].uri:find('schema.gsql', 1, true), 'go to definition jumps to the schema file')

-- Semantic tokens are applied on top of tree-sitter highlighting.
vim.wait(5000, function()
  local inspected = vim.inspect_pos(buf, 2, 11)
  return #inspected.semantic_tokens > 0
end)
local tokens = vim.inspect_pos(buf, 2, 11).semantic_tokens
check(#tokens > 0 and tokens[1].opts.hl_group:find('parameter', 1, true), 'semantic token marks `p` as a parameter')

-- Formatting re-indents the misplaced PRINT line (using the buffer's indent options).
vim.bo[buf].expandtab = true
vim.bo[buf].shiftwidth = 2
vim.lsp.buf.format({ bufnr = buf, async = false, timeout_ms = 5000 })
check(vim.api.nvim_buf_get_lines(buf, 6, 7, false)[1] == '  PRINT Result, @@count;', 'formatting fixes indentation')

-- Rename the accumulator everywhere.
local edit = request('textDocument/rename', vim.tbl_extend('force', position(1, 18), { newName = 'total' }))
if edit then
  vim.lsp.util.apply_workspace_edit(edit, client.offset_encoding)
end
local text = table.concat(vim.api.nvim_buf_get_lines(buf, 0, -1, false), '\n')
local _, renamed = text:gsub('@@total', '')
check(renamed == 3, 'rename updates every occurrence (' .. renamed .. ')')

-- Folding from the tree-sitter folds query.
vim.wo.foldmethod = 'expr'
vim.wo.foldexpr = 'v:lua.vim.treesitter.foldexpr()'
vim.cmd('normal! zx')
check(vim.fn.foldlevel(2) >= 1, 'tree-sitter folds cover the query body')

-- Call hierarchy starts at the query.
local items = request('textDocument/prepareCallHierarchy', position(0, 14))
check(items and items[1] and items[1].name == 'friends', 'call hierarchy starts at the query')

-- The GSQL Style Guide: the ftplugin indents by 4 spaces, the server hints at lower-case
-- keywords and `#` comments (hints only), "fix all" applies the fixes, and formatting
-- uses the buffer's own indentation.
local style_file = root .. '/style.gsql'
vim.fn.writefile({
  'create query style_check() for graph Social {',
  'print 1; # one',
  '}',
}, style_file)
vim.cmd('edit ' .. style_file)
local sbuf = vim.api.nvim_get_current_buf()
check(
  vim.bo[sbuf].filetype == 'gsql' and vim.bo[sbuf].shiftwidth == 4 and vim.bo[sbuf].softtabstop == 4 and vim.bo[sbuf].expandtab,
  'the ftplugin indents by 4 spaces'
)
vim.wait(10000, function()
  return #vim.lsp.get_clients({ bufnr = sbuf, name = 'gsql_lsp' }) > 0
end)
vim.wait(10000, function()
  return #vim.diagnostic.get(sbuf) > 0
end)
local codes, only_hints = {}, true
for _, d in ipairs(vim.diagnostic.get(sbuf)) do
  codes[d.code] = (codes[d.code] or 0) + 1
  only_hints = only_hints and d.severity == vim.diagnostic.severity.HINT
end
check(
  (codes['keyword-case'] or 0) >= 4 and codes['hash-comment'] == 1 and only_hints,
  'lower-case keywords and `#` comments are hinted at: ' .. vim.inspect(codes)
)
vim.lsp.buf.code_action({ bufnr = sbuf, context = { only = { 'source.fixAll' } }, apply = true })
vim.wait(5000, function()
  return vim.api.nvim_buf_get_lines(sbuf, 0, 1, false)[1]:find('CREATE', 1, true) ~= nil
end)
check(
  vim.deep_equal(vim.api.nvim_buf_get_lines(sbuf, 0, -1, false), {
    'CREATE QUERY style_check() FOR GRAPH Social {',
    'PRINT 1; // one',
    '}',
  }),
  '"fix all" upper-cases the keywords and turns `#` into `//`: ' .. vim.inspect(vim.api.nvim_buf_get_lines(sbuf, 0, -1, false))
)
vim.lsp.buf.format({ bufnr = sbuf, async = false, timeout_ms = 5000 })
check(vim.api.nvim_buf_get_lines(sbuf, 1, 2, false)[1] == '    PRINT 1; // one', 'formatting indents by the ftplugin\'s 4 spaces')

-- :checkhealth gsql (opens its own buffer, so it runs last).
vim.cmd('checkhealth gsql')
local report = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
check(
  report:find('language server: gsql-lsp', 1, true) ~= nil and report:find('from setup', 1, true) ~= nil and report:find('parser for gsql is installed', 1, true) ~= nil,
  ':checkhealth gsql finds the server and the parser'
)

finish()
