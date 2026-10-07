CREATE QUERY pr(VERTEX<Person> p, FLOAT damping = 0.85) FOR GRAPH Social {
  ^CREATE keyword
  ^QUERY keyword.function
  ^pr function
  ^VERTEX type.builtin
  ^Person type
  ^p variable.parameter
  ^FLOAT type.builtin
  ^0.85 number.float
  ^Social module
  MaxAccum<FLOAT> @@max_diff = 9999;
  ^MaxAccum type.builtin
  ^FLOAT type.builtin
  ^@@max_diff variable.member
  ^9999 number
  Start = {Person.*};
  ^Start variable
  ^Person type
  ^* character.special
  WHILE @@max_diff > 0.001 LIMIT 20 DO
  ^WHILE keyword.repeat
  ^LIMIT keyword.repeat
  ^DO keyword.repeat
    Start = SELECT s FROM Start:s -(Follows>:e)- Person:t
  ^SELECT keyword
  ^FROM keyword
  ^Follows type
  ^e variable
  ^Person type
            ACCUM t.@score += s.@score / s.outdegree("Follows")
  ^ACCUM keyword
  ^@score variable.member
  ^outdegree function.method.call
  ^"Follows" string
            POST-ACCUM @@max_diff += abs(s.@score - s.@score');
  ^POST-ACCUM keyword
  ^abs function.builtin
  ^' operator
  END;
  ^END keyword.repeat
  IF @@max_diff < 0 THEN PRINT "x"; ELSE PRINT "y"; END;
  ^IF keyword.conditional
  ^THEN keyword.conditional
  ^PRINT keyword
  ^ELSE keyword.conditional
  ^END keyword.conditional
  x = GSQL_INT_MAX * 2;
  ^GSQL_INT_MAX constant.builtin
  ^* operator
  // a comment
  ^// comment
}
CREATE QUERY near(FILE out) SYNTAX v3 {
  ^FILE type.builtin
  CREATE DIRECTED VIRTUAL EDGE Near (FROM Person, TO Person);
  ^VIRTUAL keyword.modifier
  ^Near type.definition
  SELECT m INTO T FROM (n:Person {name: "Adam"})-[:_*1..2]-(m);
  ^name variable.member
  ^_ character.special
  PRINT ~@@bits, range(1, 3) WITH VECTOR;
  ^~ operator
  ^range function.builtin
  ^VECTOR keyword
}
