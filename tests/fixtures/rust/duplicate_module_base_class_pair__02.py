class Base:
    pass


def make_socket():
    import socket
    return socket.socket()


class ConfigB(Base):
    MODE = "prod"
    TIMEOUT = 60

    def connect(self):
        return make_socket()

    def close(self, sock):
        return sock.close()

    def peek(self, sock):
        return sock