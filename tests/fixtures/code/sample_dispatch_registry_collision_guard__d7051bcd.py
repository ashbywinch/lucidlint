def _search_people(args):
    return "existing"

def run_tool(tool, args):
    if tool == "search_people":
        q = args.get("query", "")
        return {"hits": [q]}
    if tool == "person":
        return {"id": "x"}
    if tool == "relationships":
        return {"rels": []}
    return {"error": "unknown tool"}
