-- Compiles the nvim-treesitter query drafts against the generated parser and
-- checks highlight captures against the list in nvim-treesitter's CONTRIBUTING.md.
--   nvim --headless --clean -u NONE -l packaging/upstream/test_treesitter.lua
-- Env: GSQL_PARSER (default /tmp/upstream-ts/gsql.so; build with
--   cc -shared -fPIC -I tree-sitter-gsql/src tree-sitter-gsql/src/parser.c -o /tmp/upstream-ts/gsql.so),
--      NVIM_TS (default: lazy.nvim's nvim-treesitter checkout).
local root = vim.fs.dirname(vim.fs.normalize(debug.getinfo(1, 'S').source:sub(2)))
local parser = vim.env.GSQL_PARSER or '/tmp/upstream-ts/gsql.so'
local nvim_ts = vim.env.NVIM_TS or vim.fn.expand('~/.local/share/nvim/lazy/nvim-treesitter')
if vim.fn.isdirectory(nvim_ts) == 0 then
  io.stderr:write('nvim-treesitter not found at ' .. nvim_ts .. ': set NVIM_TS\n')
  os.exit(2)
end
if vim.fn.filereadable(parser) == 0 then
  io.stderr:write('parser not found at ' .. parser .. ': build it (see packaging/upstream/README.md) or set GSQL_PARSER\n')
  os.exit(2)
end
local qdir = root .. '/nvim-treesitter/runtime/queries/gsql'

local failed = false
local function fail(msg)
  io.stderr:write('FAIL: ' .. msg .. '\n')
  failed = true
end

assert(vim.uv.fs_stat(parser), 'parser not built: ' .. parser)
assert(vim.treesitter.language.add('gsql', { path = parser }), 'language.add')

for _, name in ipairs({ 'highlights', 'injections', 'folds', 'indents', 'locals' }) do
  local f = qdir .. '/' .. name .. '.scm'
  local text = table.concat(vim.fn.readfile(f), '\n')
  local ok, q = pcall(vim.treesitter.query.parse, 'gsql', text)
  if ok then
    print(('ok   %-11s %d captures'):format(name, #q.captures))
  else
    fail(name .. ': ' .. tostring(q))
  end
end

-- Capture names allowed by nvim-treesitter (CONTRIBUTING.md, "Highlights").
local allowed = {}
local contrib = table.concat(vim.fn.readfile(nvim_ts .. '/CONTRIBUTING.md'), '\n')
for cap in contrib:gmatch('@([%w_.]+)') do
  allowed[cap] = true
end
local hl = vim.treesitter.query.parse('gsql', table.concat(vim.fn.readfile(qdir .. '/highlights.scm'), '\n'))
local bad = {}
for _, cap in ipairs(hl.captures) do
  if not allowed[cap] and not cap:match('^_') then
    bad[#bad + 1] = cap
  end
end
if #bad > 0 then
  fail('captures not listed in nvim-treesitter CONTRIBUTING.md: ' .. table.concat(bad, ', '))
end

-- Highlight a sample so that the query is exercised on a real tree.
local sample = root .. '/../../examples'
local files = vim.fn.glob(sample .. '/**/*.gsql', false, true)
local hits = 0
for _, file in ipairs(files) do
  local src = table.concat(vim.fn.readfile(file), '\n')
  local tree = vim.treesitter.get_string_parser(src, 'gsql'):parse()[1]
  for _ in hl:iter_captures(tree:root(), src) do
    hits = hits + 1
  end
  if tree:root():has_error() then
    fail('parse error in ' .. file)
  end
end
print(('ok   %d example files, %d highlight captures'):format(#files, hits))
if #files == 0 or hits == 0 then
  fail('no examples highlighted')
end

-- Splice the parsers.lua entry into a copy of the real file: it must load,
-- use only InstallInfo/ParserInfo fields and keep the table sorted.
local entry = table.concat(vim.fn.readfile(root .. '/nvim-treesitter/parsers.lua.entry'), '\n') .. '\n'
local real = table.concat(vim.fn.readfile(nvim_ts .. '/lua/nvim-treesitter/parsers.lua'), '\n') .. '\n'
local a, b = real:find('  gstlaunch = {', 1, true)
local merged = a and (real:sub(1, a - 1) .. entry .. real:sub(a)) or nil
if not merged then
  fail('anchor gstlaunch not found in parsers.lua')
else
  local chunk, err = loadstring(merged)
  if not chunk then
    fail('merged parsers.lua does not load: ' .. err)
  else
    local parsers = chunk()
    local p = parsers.gsql
    local fields = { url = 1, revision = 1, branch = 1, location = 1, generate = 1, generate_from_json = 1, path = 1, queries = 1 }
    for k in pairs(p.install_info) do
      if not fields[k] then
        fail('unknown install_info field ' .. k)
      end
    end
    for k in pairs(p) do
      if not ({ install_info = 1, requires = 1, tier = 1, readme_note = 1 })[k] then
        fail('unknown parser field ' .. k)
      end
    end
    if not (p.install_info.url and p.install_info.revision and p.tier) then
      fail('missing url/revision/tier')
    end
    -- Keys in source order must be sorted (gsql goes after groq, before gstlaunch).
    local order = {}
    for name in merged:gmatch('\n  ([%w_]+) = {') do
      order[#order + 1] = name
    end
    for i = 2, #order do
      if order[i - 1] > order[i] then
        fail(('parsers.lua order: %s before %s'):format(order[i - 1], order[i]))
      end
    end
    -- tree-sitter-gsql/src/parser.c must exist at install_info.location.
    if not vim.uv.fs_stat(root .. '/../../' .. p.install_info.location .. '/src/parser.c') then
      fail('no parser.c at location ' .. p.install_info.location)
    end
    print('ok   parsers.lua entry merges, sorted, location has src/parser.c')
  end
end

if failed then
  vim.cmd('cquit 1')
end
print('OK')
vim.cmd('quit')
