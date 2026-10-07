-- Neovim declares Markdown + plain text and hierarchical document symbols; the
-- server must answer with Markdown hover and DocumentSymbol[] (with children).
--
--   GSQL_LSP_BIN=target/debug/gsql-lsp nvim --headless --clean -u NONE -l editors/neovim/test/capabilities.lua

local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.opt.rtp:prepend(plugin_dir)
vim.cmd('runtime! ftdetect/gsql.lua')

local failures = 0
local function check(condition, message)
  if condition then
    print('ok   ' .. message)
  else
    failures = failures + 1
    print('FAIL ' .. message)
  end
end

local root = vim.fn.tempname()
vim.fn.mkdir(root .. '/.git', 'p')
vim.fn.writefile({
  'CREATE QUERY q() {',
  '  SumAccum<INT> @@count;',
  '  PRINT @@count;',
  '}',
}, root .. '/q.gsql')

local bin = vim.fn.fnamemodify(vim.env.GSQL_LSP_BIN or 'gsql-lsp', ':p')
vim.cmd('edit ' .. root .. '/q.gsql')
local buf = vim.api.nvim_get_current_buf()
check(vim.bo[buf].filetype == 'gsql', 'filetype is detected')
local client_id = vim.lsp.start({ name = 'gsql_lsp', cmd = { bin }, root_dir = root }, { bufnr = buf })
check(client_id ~= nil, 'the server starts')
vim.wait(10000, function()
  local c = client_id and vim.lsp.get_client_by_id(client_id)
  return c ~= nil and c.initialized
end)

local client = vim.lsp.get_client_by_id(client_id)
local doc = vim.lsp.protocol.make_client_capabilities().textDocument
check(client ~= nil and doc.documentSymbol.hierarchicalDocumentSymbolSupport == true, 'Neovim declares hierarchical symbols')

local function request(method, params)
  local responses = vim.lsp.buf_request_sync(buf, method, params, 10000)
  local entry = responses and responses[client_id]
  return entry and entry.result
end
local uri = vim.uri_from_bufnr(buf)

local symbols = request('textDocument/documentSymbol', { textDocument = { uri = uri } })
check(symbols and symbols[1] and symbols[1].selectionRange ~= nil, 'document symbols are hierarchical (DocumentSymbol)')
check(symbols and symbols[1] and symbols[1].location == nil, 'no flat SymbolInformation')
check(symbols and symbols[1] and symbols[1].children and #symbols[1].children > 0, 'the query has children')

local hover = request('textDocument/hover', { textDocument = { uri = uri }, position = { line = 2, character = 10 } })
check(hover and hover.contents.kind == 'markdown', 'hover is Markdown')
check(hover and hover.contents.value:find('```', 1, true) ~= nil, 'hover has a code fence')

print(failures == 0 and 'all checks passed' or (failures .. ' check(s) failed'))
vim.cmd(failures == 0 and 'qall!' or 'cquit!')
