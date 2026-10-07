CREATE DIRECTED EDGE Follows (FROM Person, TO Person, since DATETIME) WITH REVERSE_EDGE="followed_by"
  ^DIRECTED keyword.modifier
  ^EDGE keyword
  ^Follows type.definition
  ^Person type
  ^since variable.member
  ^DATETIME type.builtin
  ^REVERSE_EDGE property
  ^"followed_by" string
CREATE LOADING JOB load FOR GRAPH Social {
  LOAD f TO VERTEX Person VALUES ($0, _) USING SEPARATOR=",";
  ^LOAD keyword
  ^Person type
  ^$0 variable.builtin
  ^_ character.special
  ^SEPARATOR property
}
RUN QUERY pr("x")
  ^RUN keyword
  ^pr function.call
CREATE VERTEX User (PRIMARY_ID id UINT, age UINT NULLABLE)
  ^NULLABLE keyword.modifier
CREATE GRAPH Mixed AS Social(User:public&vip) WITH ADMIN alice
  ^Mixed module
  ^Social module
  ^User type
  ^public label
  ^ADMIN keyword
