class PaymentsHandler:
    def __init__(self, svc):
        self.svc = svc
    def handle(self, evt):
        return self.svc.process(evt)
