" Checks syntax/gsql.vim:  vim -N -u NONE -es -S editors/vim/test/syntax.vim
set nocompatible
let &rtp = expand('<sfile>:p:h:h') . ',' . &rtp
filetype plugin on
syntax on
execute 'edit ' . expand('<sfile>:p:h') . '/sample.gsql'
" [line, token, expected syntax group]
let checks = [
  \ [1, 'CREATE', 'gsqlKeyword'], [1, 'QUERY', 'gsqlKeyword'], [1, 'VERTEX', 'gsqlType'], [1, 'FLOAT', 'gsqlType'],
  \ [1, '0.85', 'gsqlNumber'], [2, 'MaxAccum', 'gsqlAccumulatorType'], [2, '@@max_diff', 'gsqlGlobalAccumulator'],
  \ [3, '@score', 'gsqlLocalAccumulator'], [5, 'WHILE', 'gsqlRepeat'], [5, 'LIMIT', 'gsqlKeyword'],
  \ [7, 'outdegree', 'gsqlFunction'], [7, '"Follows"', 'gsqlString'], [8, 'POST-ACCUM', 'gsqlKeyword'],
  \ [9, 'abs', 'gsqlFunction'], [11, 'IF', 'gsqlConditional'], [11, 'THEN', 'gsqlConditional'],
  \ [12, 'TO_CSV', 'gsqlKeyword'], [12, 'PRINT', 'gsqlStatement'],
  \ [14, 'NULLABLE', 'gsqlKeyword'], [15, '$"indent":"length"', 'gsqlColumn'], [16, 'DeviationAccum', 'gsqlAccumulatorType'],
  \ [17, 'description', 'gsqlProperty'], [17, 'order', 'gsqlProperty'], [18, '-', ''], [19, '-OPTIMIZE', 'gsqlOption'],
  \ [20, 'PASSWORD', 'gsqlKeyword'],
  \ ]
let failures = 0
for [l, token, want] in checks
  let c = stridx(getline(l), token) + 1
  let got = synIDattr(synID(l, c, 1), 'name')
  if c == 0 || got !=# want
    let failures += 1
    echomsg 'FAIL line ' . l . ' ' . token . ': want ' . want . ' got ' . got
  endif
endfor
echomsg (len(checks) - failures) . ' passed, ' . failures . ' failed'
" Vim in -es mode prints nothing, so write the messages out.
call writefile(split(execute('messages'), "\n"), empty($VIM_TEST_OUT) ? '/dev/stdout' : $VIM_TEST_OUT)
execute failures ? 'cquit' : 'quit'
