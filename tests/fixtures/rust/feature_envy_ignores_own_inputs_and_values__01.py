class V:
    def leave_Node(self, original_node, updated_node):
        pos = self.get_metadata(original_node)
        if pos.start.line <= updated_node.end.line:
            return updated_node
        return updated_node
