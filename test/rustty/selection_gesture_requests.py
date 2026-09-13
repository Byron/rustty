"""Direct native SelectionGesture behavior, independent of application click policy."""
import itertools


def requests():
    def grid(action, x=0, y=0, *, tag='active', rectangle=False, boundaries=None, **options):
        op = {'action': 'gesture_' + action, 'point': {'tag': tag, 'x': x, 'y': y},
              'rectangle': rectangle, 'gesture': {'xpos': 5 + 10 * x, 'ypos': 10 + 20 * y, **options}}
        if boundaries is not None:
            op['boundary_codepoints'] = boundaries
        return {'op': 'grid', 'grid': op}

    def case(name, operations, cols=20, rows=6):
        return ({'id': 'grid/gesture/' + name, 'kind': 'input', 'cols': cols, 'rows': rows,
                 'operations': [op if isinstance(op, dict) else {'op': 'write', 'data': op.hex()}
                                for op in operations]}, ['terminal.selection', 'terminal.tracked'])

    text = b'alpha beta\r\n\r\nword.two_more\r\nwide\xe6\x96\x87x\r\nlast'
    observe = {'op': 'grid', 'grid': {'action': 'observe'}}
    for rectangle, click, drag, target in itertools.product(
            (False, True), (0, 5, 6, 9), (0, 5, 6, 9),
            ((1, 1), (0, 1), (2, 1), (1, 0), (1, 2), (0, 0), (4, 4), (0, 4))):
        x, y = target
        yield case(f'cell/{int(rectangle)}/{click}/{drag}/{x}-{y}', [text,
                   grid('press', 1, 1, time=0, xpos=15 + click),
                   grid('drag', x, y, xpos=5 + x * 10 + drag, rectangle=rectangle),
                   grid('release', x, y)], cols=5, rows=5)

    for index, (anchor, target) in enumerate((((0, 0), (4, 0)), ((4, 0), (0, 1)),
                                            ((0, 4), (4, 3)), ((4, 4), (0, 0)))):
        for rectangle in (False, True):
            yield case(f'edges/{index}/{int(rectangle)}', [
                grid('press', *anchor, time=0, xpos=5 + anchor[0] * 10 + 9),
                grid('drag', *target, xpos=5 + target[0] * 10, rectangle=rectangle)], cols=5, rows=5)

    for behavior, anchor, target, boundaries in itertools.product(
            ('word', 'line'), ((1, 0), (0, 1), (5, 2)),
            ((8, 0), (0, 1), (5, 2), (4, 3)), (None, [], [ord('.'), ord('_')])):
        yield case(f'{behavior}/{anchor}/{target}/{boundaries}', [text,
                   grid('press', *anchor, time=0, behaviors=[behavior] * 3, boundaries=boundaries),
                   grid('drag', *target, boundaries=boundaries, rectangle=True),
                   grid('release', *target)])

    semantic = b'\x1b]133;C\x07out1\r\n\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out2'
    for anchor, target in itertools.product(((1, 0), (1, 2)), ((2, 0), (1, 1), (2, 2), (0, 5))):
        yield case(f'output/{anchor}/{target}', [semantic,
                   grid('press', *anchor, time=0, behaviors=['output', 'word', 'line']),
                   grid('drag', *target)])

    for index, (times, options) in enumerate((
            ([0, 1, 2, 3, 4], {}), ([None, 1, 2], {}), ([0, None, 2], {}),
            ([10, 9, 9], {}), ([0, 500_000_000, 1_000_000_001], {}),
            ([0, 1, 1], {'repeat_interval': 0}),
            ([-(2**63), 2**63 - 1], {'repeat_interval': 2**64 - 1}),
            ([0, 1, 2], {'behaviors': ['line', 'word', 'output']}))):
        operations = [text]
        for time in times:
            operations.extend((grid('press', 1, 0, time=time, **options), grid('release', 1, 0)))
        yield case(f'repeat/time/{index}', operations)
    for distance in (12.9, 13.0, 13.1, -1.0):
        yield case(f'repeat/distance/{distance}', [text, grid('press', 1, 0, time=0, xpos=10, ypos=10),
                   grid('release', 1, 0), grid('press', 2, 0, time=1, xpos=15, ypos=22, max_distance=distance),
                   grid('press', 2, 0, time=2, xpos=20, ypos=22, max_distance=distance)])

    for xpos in (-1e300, -1.0, 0.0, 5.0, 1e300):
        for geometry in ({'columns': 0, 'cell_width': 10, 'padding_left': 5, 'screen_height': 100},
                         {'columns': 5, 'cell_width': 0, 'padding_left': 5, 'screen_height': 100},
                         {'columns': 2**32 - 1, 'cell_width': 2**32 - 1, 'padding_left': 0, 'screen_height': 0},
                         {'columns': 5, 'cell_width': 10, 'padding_left': 2**32 - 1, 'screen_height': 100}):
            yield case(f'geometry/{xpos}/{geometry}', [grid('press', 1, 1, time=0),
                       grid('drag', 1, 1, xpos=xpos, geometry=geometry)], cols=5, rows=5)

    for y in (-10, 1, 1.1, 99, 99.1, 110):
        yield case(f'autoscroll/edge/{y}', [text, grid('press', 1, 0, time=0),
                   grid('drag', 4, 0, ypos=y), grid('release', 4, 0),
                   grid('autoscroll', 4, 0, tag='viewport', ypos=y)])
    history = b'zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven'
    for direction, y, row in (('up', 0, 0), ('down', 101, 4)):
        operations = [history, {'op': 'grid', 'grid': {'action': 'viewport', 'delta': -2}},
                      grid('press', 1, 1, tag='viewport', time=0),
                      grid('drag', 3, row, tag='viewport', ypos=y)]
        operations.extend(grid('autoscroll', 3, row, tag='viewport', ypos=y) for _ in range(6))
        operations.extend((grid('release', 3, row, tag='viewport'), grid('autoscroll', 3, row, tag='viewport', ypos=y)))
        yield case(f'autoscroll/{direction}', operations, cols=10, rows=5)
    yield case('autoscroll/invalid-viewport', [history, grid('press', 1, 0, time=0),
               grid('drag', 2, 0, ypos=0), grid('autoscroll', 65535, 999, tag='viewport', ypos=0)])

    for action in ('drag', 'deep_press', 'release', 'autoscroll', 'reset'):
        yield case(f'idle/{action}', [text, grid(action, 1, 0), observe])
    for anchor in ((1, 0), (0, 1)):
        yield case(f'deep/{anchor}', [text, grid('press', *anchor, time=0),
                   grid('drag', *anchor, ypos=0), grid('deep_press'),
                   grid('drag', 8, 2, ypos=101), grid('autoscroll', 8, 2),
                   grid('release', 8, 2), grid('press', *anchor, time=1)])
    yield case('reset', [text, grid('press', 1, 0, time=0), grid('drag', 8, 0, ypos=0),
               grid('reset'), grid('press', 1, 0, time=1), grid('release', 65535, 999)])
    for name, mutation in (
            ('output', [b'\r\neight\r\nnine']),
            ('viewport', [{'op': 'grid', 'grid': {'action': 'viewport', 'delta': -2}}]),
            ('reflow', [{'op': 'resize', 'cols': 7, 'rows': 5}]),
            ('switch', [b'\x1b[?1049h']),
            ('switch-back', [b'\x1b[?1049h\x1b[?1049l']),
            ('reset', [{'op': 'terminal_reset'}])):
        yield case(f'lifetime/{name}', [history, grid('press', 1, 1, time=0), *mutation, observe,
                   grid('drag', 2, 1, ypos=0), grid('deep_press'), grid('release', 2, 1),
                   grid('press', 1, 1, time=1)], cols=10, rows=5)
    yield case('lifetime/alternate-cleared', [b'\x1b[?1049h', text, grid('press', 1, 0, time=0),
               b'\x1b[?1049l\x1b[?1049h', observe, grid('drag', 2, 0, ypos=0),
               grid('autoscroll', 2, 0, ypos=0), grid('press', 1, 0, time=1)])
    yield case('lifetime/alternate-recycled', [b'\x1b[?1049h', text, grid('press', 1, 0, time=0),
               {'op': 'terminal_reset'}, b'\x1b[?1049h', observe, grid('drag', 2, 0, ypos=0),
               grid('autoscroll', 2, 0, ypos=0), grid('press', 1, 0, time=1)])
    yield case('lifetime/autoscroll-screen-change', [text, grid('press', 1, 0, time=0),
               grid('drag', 2, 0, ypos=0), b'\x1b[?1049h', grid('autoscroll', 2, 0, ypos=0), observe])
    yield case('lifetime/pruned-repeat', [{'op': 'grid', 'grid': {'action': 'limits', 'lines': 0, 'bytes': 0}},
               grid('press', 1, 0, time=0), history, observe, grid('release', 1, 0),
               grid('press', 1, 0, time=1), grid('reset')], cols=10, rows=5)
    for name, mutation in (('reset', [{'op': 'terminal_reset'}]),
                           ('pruned-page', [b'line\r\n' * 1024])):
        setup = [{'op': 'grid', 'grid': {'action': 'limits', 'bytes': 1024}},
                 grid('press', 1, 0, time=0), grid('drag', 2, 0, ypos=0), *mutation, observe]
        yield case(f'lifetime/{name}-repeat', [*setup, grid('release', 1, 0),
                   grid('press', 1, 0, time=1), grid('drag', 2, 0)], cols=80, rows=5)
        yield case(f'lifetime/{name}-autoscroll', [*setup, grid('drag', 2, 0),
                   grid('deep_press'), grid('autoscroll', 2, 0, ypos=0), observe], cols=80, rows=5)
