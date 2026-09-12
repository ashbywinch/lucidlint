class _NoRedirect:
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None
