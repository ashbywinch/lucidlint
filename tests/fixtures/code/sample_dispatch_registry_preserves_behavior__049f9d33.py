def run_tool(tool, args, facts):
    people = facts["people"]
    if tool == "search_people":
        q = str(args.get("query", "")).strip().lower()
        return {"hits": [p for p in people if q in p.lower()]}
    if tool == "person":
        return {"id": str(args.get("id", ""))}
    if tool == "relationships":
        return {"rels": [r for r in facts["rels"] if r.get("a") == args.get("id")]}
    return {"error": "unknown tool"}
