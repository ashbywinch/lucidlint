def b(order, user):
    return order is not None and order.total > 0 and order.kind != "draft" and not user.locked
