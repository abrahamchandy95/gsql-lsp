# Algorithm examples

Graph algorithms written in the style of TigerGraph's Graph Data Science library.
They take vertex and edge type names as `STRING`/`SET<STRING>` parameters, so they
run on any graph:

```
INSTALL QUERY tg_pagerank
RUN QUERY tg_pagerank("Person", "Follows", 0.001, 25, 0.85, 10)
```
